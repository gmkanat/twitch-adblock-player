# README screenshot fixtures

These NASA images are used only to generate the fictional application state in
the README screenshot:

- `earth-orbit.jpg`: NASA image `iss071e456772`
- `atmosphere.jpg`: NASA image `iss071e364425`
- `city-lights.jpg`: NASA image `iss072e518202`
- `mission-control.jpg`: NASA image `MSFC-202100043`

Sources: <https://images.nasa.gov/>. See NASA's
[media usage guidelines](https://www.nasa.gov/nasa-brand-center/images-and-media/).

Regenerate the screenshot from the repository root with:

```sh
node desktop/tests/readme-screenshot.mjs
```
