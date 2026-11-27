# R1 failure triage

With the glyph, table and paint sampling fixes, the JS runtime passes
**160/161 (99.4%)** on RADV and lavapipe with zero render errors.
`09-content/04` passes with nearest sampling for
`image-rendering: pixelated`. The only remaining case is
`08-effects/06-mix-blend-mode-multiply-screen`, where the rounded circle
edges still differ from Chromium's antialiasing.
