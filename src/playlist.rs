//! Pure (no-IO) HLS playlist logic: parsing Twitch's master playlist and
//! stripping server-stitched ad segments out of a live media playlist.
//!
//! This module stays side-effect free; network and process IO live in
//! `playback.rs`.

use anyhow::{bail, Result};

/// One quality option from the Twitch master playlist.
#[derive(Debug)]
pub struct Variant {
    /// Friendly name Twitch assigns, e.g. `1080p60`, `720p60`, `audio_only`.
    pub name: String,
    /// Advertised bitrate in bits/sec.
    pub bandwidth: u64,
    /// URL of this variant's media playlist (the per-quality `.m3u8`).
    pub url: String,
    /// True for the original/source rendition (`IVS-VARIANT-SOURCE="source"`).
    pub source: bool,
}

/// Result of filtering a media playlist.
#[derive(Debug, Default)]
pub struct StripResult {
    /// The cleaned playlist text, ready to hand to a player.
    pub playlist: String,
    /// How many ad segments were dropped.
    pub removed_segments: usize,
    /// How many real (non-ad) media segments remain — if this is 0 the playlist
    /// is unplayable (whole thing was an ad, e.g. pre-roll) and we must swap.
    pub kept_segments: usize,
}

/// Parse the master playlist into the list of selectable quality variants.
pub fn parse_master(master: &str) -> Vec<Variant> {
    let lines: Vec<&str> = master.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].starts_with("#EXT-X-STREAM-INF") {
            let inf = lines[i];
            let bandwidth = attr_num(inf, "BANDWIDTH").unwrap_or(0);
            // Twitch's current format names variants via STABLE-VARIANT-ID / IVS-NAME;
            // older formats used VIDEO. Fall back to bitrate if none are present.
            let name = attr_str(inf, "STABLE-VARIANT-ID")
                .or_else(|| attr_str(inf, "IVS-NAME"))
                .or_else(|| attr_str(inf, "VIDEO"))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("{}kbps", bandwidth / 1000));
            let source = attr_str(inf, "IVS-VARIANT-SOURCE").as_deref() == Some("source");
            // The variant URL is the next line that is neither blank nor a tag.
            let mut j = i + 1;
            while j < lines.len() && (lines[j].trim().is_empty() || lines[j].starts_with('#')) {
                j += 1;
            }
            if j < lines.len() {
                out.push(Variant {
                    name,
                    bandwidth,
                    url: lines[j].trim().to_string(),
                    source,
                });
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Choose a variant given a user quality string (`best`/`source`/`worst`/`audio_only`/`720p60`/…).
pub fn select_variant<'a>(variants: &'a [Variant], quality: &str) -> Result<&'a Variant> {
    if variants.is_empty() {
        bail!("master playlist has no variants (stream offline?)");
    }
    let q = quality.trim().to_ascii_lowercase();

    // Highest-bandwidth *video* rendition (never silently fall back to audio_only).
    let best_video = || {
        variants
            .iter()
            .filter(|v| !v.name.to_ascii_lowercase().contains("audio"))
            .max_by_key(|v| v.bandwidth)
            .or_else(|| variants.iter().max_by_key(|v| v.bandwidth))
    };

    if q == "best" {
        return best_video().ok_or_else(|| anyhow::anyhow!("master playlist has no variants"));
    }
    if q == "source" || q == "chunked" {
        // Prefer the flagged source rendition, but it's not always present/correct.
        return variants
            .iter()
            .find(|v| v.source)
            .or_else(best_video)
            .ok_or_else(|| anyhow::anyhow!("master playlist has no variants"));
    }
    if q == "worst" {
        return variants
            .iter()
            .min_by_key(|v| v.bandwidth)
            .ok_or_else(|| anyhow::anyhow!("master playlist has no variants"));
    }
    if let Some(v) = variants.iter().find(|v| v.name.eq_ignore_ascii_case(&q)) {
        return Ok(v);
    }
    if let Some(v) = variants
        .iter()
        .find(|v| v.name.to_ascii_lowercase().contains(&q))
    {
        return Ok(v);
    }
    bail!(
        "quality '{}' not found. available: {}",
        quality,
        variants
            .iter()
            .map(|v| v.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
}

/// Remove server-stitched (SSAI) ad segments from a live media playlist.
///
/// Detection rules (matching how Streamlink/Twitch tag ads):
///   * `#EXT-X-DATERANGE` lines whose `CLASS="twitch-stitched-ad"`, whose
///     `ID` starts with `stitched-ad-`, or that carry `X-TV-TWITCH-AD-*`
///     attributes are dropped.
///   * Any media segment whose `#EXTINF` title contains `Amazon` is an ad
///     segment — the `#EXTINF` line and its following URI line are dropped.
pub fn strip_ads(media: &str) -> StripResult {
    let lines: Vec<&str> = media.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut removed = 0usize;
    let mut kept = 0usize;
    let mut leading_removed = 0usize; // ad segments dropped before any kept segment
    let mut seq_idx: Option<usize> = None; // index of the MEDIA-SEQUENCE line in `out`
    let mut seq_value: u64 = 0;

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];

        // Remember the media-sequence header so we can fix it after stripping.
        if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            seq_value = rest.trim().parse().unwrap_or(0);
            seq_idx = Some(out.len());
            out.push(line.to_string());
            i += 1;
            continue;
        }

        // Drop the ad-announcing daterange tags entirely.
        if line.starts_with("#EXT-X-DATERANGE") && is_ad_daterange(line) {
            i += 1;
            continue;
        }

        // A segment is `#EXTINF:<dur>,<title>` immediately followed by its URI line.
        if line.starts_with("#EXTINF") {
            let uri = lines.get(i + 1).copied().unwrap_or("");
            if is_ad_extinf(line) {
                removed += 1;
                if kept == 0 {
                    leading_removed += 1;
                }
                i += 2; // skip both the #EXTINF and the URI
                continue;
            }
            kept += 1;
            out.push(line.to_string());
            if i + 1 < lines.len() {
                out.push(uri.to_string());
            }
            i += 2;
            continue;
        }

        out.push(line.to_string());
        i += 1;
    }

    // Dropping leading ad segments shifts the numbering of every segment after
    // them, so the MEDIA-SEQUENCE header must advance by that many, or the
    // player reports "media sequence changed unexpectedly" and stalls.
    if leading_removed > 0 {
        if let Some(idx) = seq_idx {
            out[idx] = format!(
                "#EXT-X-MEDIA-SEQUENCE:{}",
                seq_value + leading_removed as u64
            );
        }
    }

    let mut playlist = out.join("\n");
    if media.ends_with('\n') {
        playlist.push('\n');
    }
    StripResult {
        playlist,
        removed_segments: removed,
        kept_segments: kept,
    }
}

fn is_ad_daterange(line: &str) -> bool {
    line.contains("twitch-stitched-ad")
        || line.contains("stitched-ad-")
        || line.contains("X-TV-TWITCH-AD")
}

fn is_ad_extinf(line: &str) -> bool {
    match line.find(',') {
        Some(idx) => line[idx + 1..].to_ascii_lowercase().contains("amazon"),
        None => false,
    }
}

/// Read an unquoted numeric attribute (e.g. `BANDWIDTH=6000000`), making sure
/// the match is a real attribute start (preceded by `:` or `,`) so that
/// `AVERAGE-BANDWIDTH` doesn't get mistaken for `BANDWIDTH`.
fn attr_num(line: &str, key: &str) -> Option<u64> {
    let needle = format!("{key}=");
    let mut from = 0;
    while let Some(pos) = line[from..].find(&needle) {
        let abs = from + pos;
        let prev = if abs == 0 {
            None
        } else {
            line[..abs].chars().last()
        };
        if abs == 0 || matches!(prev, Some(':') | Some(',')) {
            let start = abs + needle.len();
            let rest = &line[start..];
            let end = rest.find(',').unwrap_or(rest.len());
            return rest[..end].trim().parse().ok();
        }
        from = abs + needle.len();
    }
    None
}

/// Read a quoted string attribute (e.g. `VIDEO="chunked"`).
fn attr_str(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real Twitch master-playlist shape (note: source variant is listed LAST,
    // names live in STABLE-VARIANT-ID, source is flagged via IVS-VARIANT-SOURCE).
    const MASTER: &str = "\
#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=3422999,RESOLUTION=1280x720,CODECS=\"avc1.4D401F,mp4a.40.2\",FRAME-RATE=60.000,STABLE-VARIANT-ID=\"720p60\",IVS-NAME=\"720p60\",IVS-VARIANT-SOURCE=\"transcode\"
https://video-weaver.example/720p60.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=160000,CODECS=\"mp4a.40.2\",STABLE-VARIANT-ID=\"audio_only\",IVS-NAME=\"audio_only\",IVS-VARIANT-SOURCE=\"transcode\"
https://video-weaver.example/audio.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=7970660,RESOLUTION=1920x1080,CODECS=\"avc1.64002A,mp4a.40.2\",FRAME-RATE=60.000,STABLE-VARIANT-ID=\"1080p60\",IVS-NAME=\"1080p60\",IVS-VARIANT-SOURCE=\"source\"
https://video-weaver.example/source.m3u8
";

    const MEDIA_WITH_AD: &str = "\
#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:2
#EXT-X-MEDIA-SEQUENCE:100
#EXT-X-TWITCH-LIVE-SEQUENCE:100
#EXT-X-PROGRAM-DATE-TIME:2026-06-23T00:00:00.000Z
#EXTINF:2.000,live
https://video-edge.example/seg100.ts
#EXT-X-DATERANGE:ID=\"stitched-ad-1\",CLASS=\"twitch-stitched-ad\",START-DATE=\"2026-06-23T00:00:02.000Z\",DURATION=4.0,X-TV-TWITCH-AD-ROLL-TYPE=\"MIDROLL\"
#EXT-X-DISCONTINUITY
#EXT-X-PROGRAM-DATE-TIME:2026-06-23T00:00:02.000Z
#EXTINF:2.000,Amazon
https://video-edge.example/ad1.ts
#EXTINF:2.000,Amazon
https://video-edge.example/ad2.ts
#EXT-X-DISCONTINUITY
#EXT-X-PROGRAM-DATE-TIME:2026-06-23T00:00:06.000Z
#EXTINF:2.000,live
https://video-edge.example/seg101.ts
";

    #[test]
    fn parses_all_variants() {
        let v = parse_master(MASTER);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].name, "720p60");
        assert_eq!(v[0].bandwidth, 3_422_999);
        assert_eq!(v[0].url, "https://video-weaver.example/720p60.m3u8");
        assert!(!v[0].source);
        assert_eq!(v[2].name, "1080p60");
        assert!(v[2].source); // flagged even though it's last
    }

    #[test]
    fn selects_quality() {
        let v = parse_master(MASTER);
        // "best" picks the source rendition, not merely the first listed.
        assert_eq!(select_variant(&v, "best").unwrap().name, "1080p60");
        assert_eq!(
            select_variant(&v, "best").unwrap().url,
            "https://video-weaver.example/source.m3u8"
        );
        assert_eq!(select_variant(&v, "worst").unwrap().name, "audio_only");
        assert_eq!(
            select_variant(&v, "720p60").unwrap().url,
            "https://video-weaver.example/720p60.m3u8"
        );
        assert_eq!(select_variant(&v, "audio").unwrap().name, "audio_only"); // partial match
        assert!(select_variant(&v, "144p").is_err());
    }

    #[test]
    fn strips_ad_segments() {
        let r = strip_ads(MEDIA_WITH_AD);
        assert_eq!(r.removed_segments, 2);
        assert_eq!(r.kept_segments, 2); // seg100 + seg101 survive
        assert!(!r.playlist.contains("ad1.ts"));
        assert!(!r.playlist.contains("ad2.ts"));
        assert!(!r.playlist.contains("twitch-stitched-ad"));
        // content survives untouched
        assert!(r.playlist.contains("seg100.ts"));
        assert!(r.playlist.contains("seg101.ts"));
        assert!(r.playlist.contains("#EXT-X-MEDIA-SEQUENCE:100"));
    }

    #[test]
    fn detects_all_ad_preroll() {
        // A pre-roll playlist: every segment is an ad → nothing playable left.
        let preroll = "\
#EXTM3U
#EXT-X-MEDIA-SEQUENCE:1
#EXT-X-DATERANGE:ID=\"stitched-ad-9\",CLASS=\"twitch-stitched-ad\",START-DATE=\"2026-06-24T00:00:00Z\",DURATION=6.0
#EXTINF:2.000,Amazon
https://e/ad1.ts
#EXTINF:2.000,Amazon
https://e/ad2.ts
#EXTINF:2.000,Amazon
https://e/ad3.ts
";
        let r = strip_ads(preroll);
        assert_eq!(r.removed_segments, 3);
        assert_eq!(r.kept_segments, 0); // signals "swap to backup stream"
    }

    #[test]
    fn bumps_media_sequence_when_leading_ads_removed() {
        // 2 ad segments at the front, then content → sequence must advance 50 → 52.
        let m = "\
#EXTM3U
#EXT-X-MEDIA-SEQUENCE:50
#EXTINF:2.000,Amazon
https://e/ad1.ts
#EXTINF:2.000,Amazon
https://e/ad2.ts
#EXTINF:2.000,live
https://e/seg52.ts
#EXTINF:2.000,live
https://e/seg53.ts
";
        let r = strip_ads(m);
        assert_eq!(r.removed_segments, 2);
        assert_eq!(r.kept_segments, 2);
        assert!(r.playlist.contains("#EXT-X-MEDIA-SEQUENCE:52"));
        assert!(!r.playlist.contains("#EXT-X-MEDIA-SEQUENCE:50"));
    }

    #[test]
    fn keeps_media_sequence_when_only_middle_ads_removed() {
        // Ads are in the middle (first segment kept) → sequence header unchanged.
        let r = strip_ads(MEDIA_WITH_AD);
        assert!(r.playlist.contains("#EXT-X-MEDIA-SEQUENCE:100"));
    }

    #[test]
    fn leaves_clean_playlist_unchanged() {
        let clean = "#EXTM3U\n#EXTINF:2.000,live\nhttps://e/seg1.ts\n";
        let r = strip_ads(clean);
        assert_eq!(r.removed_segments, 0);
        assert_eq!(r.playlist, clean);
    }
}
