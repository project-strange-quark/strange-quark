// SPDX-License-Identifier: MIT

fn text_coverage(coverage: f32, srgb: vec3f) -> f32 {
    if coverage <= 0.0 || coverage >= 1.0 {
        return coverage;
    }

    // Match Skia's Linux grayscale preblend using the text color alone, so
    // the same unmodified glyph atlas can serve different colors and layers.
    // Skia bins each 8-bit channel and the resulting luma into eight levels.
    let bins = vec3u(round(srgb * 255.0)) >> vec3u(5u);
    let channels = (bins << vec3u(5u)) | (bins << vec3u(2u)) | (bins >> vec3u(1u));
    let luma = (54u * channels.r + 183u * channels.g + 19u * channels.b) >> 13u;
    let foreground = f32((luma << 5u) | (luma << 2u) | (luma >> 1u)) / 255.0;
    let background = 1.0 - foreground;

    // Chromium uses gamma 1.2 (truncated to 2.6 fixed point) and contrast 0.2.
    const gamma = 76.0 / 64.0;
    let background_light = pow(background, gamma);
    let foreground_light = pow(foreground, gamma);
    let contrasted = coverage + 0.2 * background_light * coverage * (1.0 - coverage);
    let blended = pow(mix(background_light, foreground_light, contrasted), 1.0 / gamma);
    let corrected = clamp((blended - background) / (foreground - background), 0.0, 1.0);
    return round(corrected * 255.0) / 255.0;
}
