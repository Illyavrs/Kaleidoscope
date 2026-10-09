# Kaleidoscope GUI font notices

The GUI embeds fonts from the following npm packages. Each accompanying license file is an exact copy of the installed package's `LICENSE`, including its attribution and the full SIL Open Font License 1.1 text.

| Font | Package version | License file | Package provenance |
| --- | --- | --- | --- |
| Nunito variable | `@fontsource-variable/nunito` 5.3.0 | [Nunito.txt](licenses/Nunito.txt) | [Fontsource font-files, fonts/variable/nunito](https://github.com/fontsource/font-files/tree/main/fonts/variable/nunito); [npm package](https://www.npmjs.com/package/@fontsource-variable/nunito/v/5.3.0) |
| Noto Sans TC | `@fontsource/noto-sans-tc` 5.3.0 | [NotoSansTC.txt](licenses/NotoSansTC.txt) | [Fontsource font-files, fonts/google/noto-sans-tc](https://github.com/fontsource/font-files/tree/main/fonts/google/noto-sans-tc); [npm package](https://www.npmjs.com/package/@fontsource/noto-sans-tc/v/5.3.0) |

The packaged Nunito attribution is: “Copyright 2014 The Nunito Project Authors (https://github.com/googlefonts/nunito) Nunito-Italic[wght].ttf: Copyright 2014 The Nunito Project Authors (https://github.com/googlefonts/nunito)”. The packaged Noto Sans TC attribution is: “Google Inc.” Both packages declare `OFL-1.1`.

Versions and provenance above come from the cached packages' `package.json` and `metadata.json`, with distribution versions pinned in the Illyaverse `package-lock.json`. The UI imports Nunito's variable-weight CSS and Noto Sans TC's 400/600-weight CSS. This notice covers the GUI fonts; the connection bridge has its own third-party notices.
