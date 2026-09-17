# Third-party notices

## RustyPipe

This build links the GPL-3.0 RustyPipe library. Source revision, local compatibility
changes and full license are included under `vendor/rustypipe/`. The combined
native library and CLI are distributed under GPL-3.0. The original project
source remains available under the MIT notice in `LICENSE-MIT`; that notice does
not replace RustyPipe's distribution requirements. Source release archives
include the locked dependency sources and offline Cargo configuration.

Copyright (c) ThetaDev and RustyPipe contributors.


## GoogleVideo protocol reference

The native SABR/UMP implementation in `src/sabr/` uses the protocol field
definitions and UMP framing documented by [LuanRT/googlevideo](https://github.com/LuanRT/googlevideo).
Reference revision: `58f92b7ba8fc252a510963f003088279a00d4ab0`.
No JavaScript runtime from that project is required.

MIT License

Copyright (c) 2024 LuanRT

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

## Official-browser PO token protocol research

The independently written provider in `src/attestation_browser.js` follows the
BotGuard/WAA protocol research published by
[BgUtils](https://github.com/LuanRT/BgUtils), reference revision
`84e3705ccbbf1224c8df0502fdf2c712a666b04f`. No BgUtils runtime is bundled; the
provider executes Google's own interpreter inside an official Music page.

WEB_REMIX player versus GVS content-binding rules were cross-checked against
[`yt-dlp`'s PO token utilities](https://github.com/yt-dlp/yt-dlp/blob/c7fb478d21e9e59524befbe23f7801bb267fb880/yt_dlp/extractor/youtube/pot/utils.py)
(Unlicense). No Python code or yt-dlp runtime is bundled by this provider.

BgUtils is distributed under the following license:

MIT License

Copyright (c) 2024 LuanRT

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
