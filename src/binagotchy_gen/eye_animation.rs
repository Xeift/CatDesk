use super::binagotchy_utils::*;
use super::constants::*;
use super::types::*;
use image::RgbaImage;

/// 眼睛开合程度：0.0 = 完全闭合, 1.0 = 完全睁开
pub fn draw_animated_eyes(
    img: &mut RgbaImage,
    cx: i32,
    eye_y: i32,
    eye_dx: i32,
    eye_color: Color,
    openness: f32, // 0.0 到 1.0
) {
    let openness = openness.max(0.0).min(1.0);

    // 完全闭合 (0.0)
    if openness < 0.1 {
        // 画一条横线表示闭眼
        for ex in [cx - eye_dx, cx + eye_dx] {
            line(img, ex - 1, eye_y + 1, ex + 2, eye_y + 1, EYE_LN);
        }
        return;
    }

    // 半睁 (0.1 - 0.5)
    if openness < 0.5 {
        for ex in [cx - eye_dx, cx + eye_dx] {
            // 只画下半部分
            pt(img, ex, eye_y + 1, EYE_LN);
            pt(img, ex + 1, eye_y + 1, EYE_LN);
            pt(img, ex, eye_y + 2, EYE_LN);
            pt(img, ex + 1, eye_y + 2, EYE_LN);
        }
        return;
    }

    // 大部分睁开 (0.5 - 0.8)
    if openness < 0.8 {
        for ex in [cx - eye_dx, cx + eye_dx] {
            // 画两行
            pt(img, ex, eye_y, EYE_LN);
            pt(img, ex + 1, eye_y, EYE_LN);
            pt(img, ex, eye_y + 1, WHITE);
            pt(img, ex + 1, eye_y + 1, EYE_LN);
            pt(img, ex, eye_y + 2, EYE_LN);
            pt(img, ex + 1, eye_y + 2, EYE_LN);
        }
        return;
    }

    // 完全睁开 (0.8 - 1.0) - 正常眼睛
    for ex in [cx - eye_dx, cx + eye_dx] {
        for yy in 0..3 {
            pt(img, ex, eye_y + yy, EYE_LN);
            pt(img, ex + 1, eye_y + yy, EYE_LN);
        }
        pt(img, ex, eye_y, WHITE);
        pt(img, ex, eye_y + 2, eye_color);
    }
}
