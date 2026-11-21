use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// RAII temporary directory guard that deletes its directory tree on drop.
/// Zero external crate dependency (`tempfile`-free).
#[derive(Debug)]
pub struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    pub fn new(prefix: &str) -> Result<Self> {
        let dir = std::env::temp_dir().join(format!("epicenter-{prefix}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create temp dir {}", dir.display()))?;
        Ok(Self { path: dir })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Pure-Rust standard RFC 4648 base64 encoder (zero external crate dependencies).
pub fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut chunks = input.chunks_exact(3);

    for chunk in &mut chunks {
        let n = ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | (chunk[2] as u32);
        out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
        out.push(TABLE[(n & 0x3F) as usize] as char);
    }

    match chunks.remainder() {
        [a] => {
            let n = (*a as u32) << 16;
            out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
            out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
            out.push('=');
            out.push('=');
        }
        [a, b] => {
            let n = ((*a as u32) << 16) | ((*b as u32) << 8);
            out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
            out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
            out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
            out.push('=');
        }
        _ => {}
    }

    out
}

/// Renders a crisp, deterministic dark-terminal PNG screenshot depicting the real
/// `pallets/click` PR #3884 (`show_envvar`) terminal bug reproduction at commit `bb695aa`.
pub fn render_click_3884_terminal_png() -> Vec<u8> {
    const WIDTH: usize = 984;
    const HEIGHT: usize = 468;
    const SCALE: usize = 2;

    let mut canvas = RgbCanvas::new(WIDTH, HEIGHT, (13, 17, 23)); // #0D1117

    // Outer terminal window frame (#161B22) with 1px slate border (#30363D).
    let win_x = 16;
    let win_y = 14;
    let win_w = WIDTH - 32;
    let win_h = HEIGHT - 28;
    canvas.fill_rect(win_x, win_y, win_w, win_h, (22, 27, 34));
    canvas.stroke_rect(win_x, win_y, win_w, win_h, (48, 54, 61));

    // Title bar (#1F242C) + divider line.
    let title_h = 34;
    canvas.fill_rect(win_x + 1, win_y + 1, win_w - 2, title_h, (31, 36, 44));
    canvas.fill_rect(win_x + 1, win_y + title_h, win_w - 2, 1, (48, 54, 61));

    // macOS window control dots.
    canvas.fill_circle(win_x + 20, win_y + 18, 6, (255, 95, 86)); // red
    canvas.fill_circle(win_x + 40, win_y + 18, 6, (255, 189, 46)); // yellow
    canvas.fill_circle(win_x + 60, win_y + 18, 6, (39, 201, 63)); // green

    // Title bar text (1x crisp label centered vertically).
    canvas.draw_text(
        win_x + 86,
        win_y + 11,
        2,
        "pallets/click @ bb695aa - show_envvar terminal bug (PR #3884)",
        (139, 148, 158),
    );

    // Terminal body lines.
    let left_x = win_x + 18;
    let mut y = win_y + title_h + 18;
    let line_step = 21;

    // Color palette (GitHub Dark High-Contrast inspired)
    let c_prompt = (88, 166, 255); // #58A6FF
    let c_cmd = (230, 237, 243); // #E6EDF3
    let c_muted = (139, 148, 158); // #8B949E
    let c_error = (255, 123, 114); // #FF7B72
    let c_err_sub = (255, 161, 152); // #FFA198
    let c_warn = (210, 153, 34); // #D29922
    let c_ok = (126, 231, 135); // #7EE787

    // Command 1: sequence envvar error hint
    canvas.draw_text(left_x, y, SCALE, "$ ", c_prompt);
    canvas.draw_text(left_x + 24, y, SCALE, "weather --units invalid", c_cmd);
    y += line_step;

    canvas.draw_text(left_x, y, SCALE, "Usage: weather [OPTIONS]", c_muted);
    y += line_step;
    canvas.draw_text(left_x, y, SCALE, "Try 'weather --help' for help.", c_muted);
    y += line_step + 4;

    // Subtle red highlight strip behind the primary buggy error line.
    canvas.fill_rect(win_x + 8, y - 3, win_w - 16, 20, (52, 22, 26));
    canvas.draw_text(
        left_x,
        y,
        SCALE,
        "Error: Invalid value for '--units' (env var: '['WEATHER_UNITS', 'UNITS']'):",
        c_error,
    );
    y += line_step;

    canvas.draw_text(
        left_x,
        y,
        SCALE,
        "  'invalid' is not one of 'metric', 'imperial'.",
        c_err_sub,
    );
    y += line_step;

    canvas.draw_text(
        left_x,
        y,
        SCALE,
        "  ^ BUG: list repr leaked; expected (env var: 'WEATHER_UNITS', 'UNITS')",
        c_warn,
    );
    y += line_step + 10;

    // Command 2: --help with empty envvar='' and envvar=[]
    canvas.draw_text(left_x, y, SCALE, "$ ", c_prompt);
    canvas.draw_text(left_x + 24, y, SCALE, "weather --help", c_cmd);
    y += line_step;

    canvas.draw_text(left_x, y, SCALE, "Options:", c_cmd);
    y += line_step;

    canvas.draw_text(
        left_x,
        y,
        SCALE,
        "  --units [metric|imperial]  [env var: WEATHER_UNITS, UNITS]",
        c_ok,
    );
    y += line_step;

    canvas.fill_rect(win_x + 8, y - 3, win_w - 16, 20, (52, 22, 26));
    canvas.draw_text(
        left_x,
        y,
        SCALE,
        "  --token TEXT               [env var: ]",
        c_error,
    );
    canvas.draw_text(
        left_x + 40 * 12 + 12,
        y,
        SCALE,
        "<- BUG: envvar=''",
        c_warn,
    );
    y += line_step;

    canvas.fill_rect(win_x + 8, y - 3, win_w - 16, 20, (52, 22, 26));
    canvas.draw_text(
        left_x,
        y,
        SCALE,
        "  --proxy TEXT               [env var: ]",
        c_error,
    );
    canvas.draw_text(
        left_x + 40 * 12 + 12,
        y,
        SCALE,
        "<- BUG: envvar=[]",
        c_warn,
    );
    y += line_step + 10;

    // Command 3: empty envvar in error hint
    canvas.draw_text(left_x, y, SCALE, "$ ", c_prompt);
    canvas.draw_text(left_x + 24, y, SCALE, "weather --token", c_cmd);
    y += line_step;

    canvas.fill_rect(win_x + 8, y - 3, win_w - 16, 20, (52, 22, 26));
    canvas.draw_text(
        left_x,
        y,
        SCALE,
        "Error: Option '--token' requires an argument (env var: '').",
        c_error,
    );
    y += line_step;

    canvas.draw_text(
        left_x,
        y,
        SCALE,
        "  ^ BUG: empty envvar='' or [] should omit env var hint in error & help",
        c_warn,
    );

    encode_rgb_png(WIDTH as u32, HEIGHT as u32, &canvas.pixels)
}

struct RgbCanvas {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

impl RgbCanvas {
    fn new(width: usize, height: usize, bg: (u8, u8, u8)) -> Self {
        let mut pixels = Vec::with_capacity(width * height * 3);
        for _ in 0..(width * height) {
            pixels.push(bg.0);
            pixels.push(bg.1);
            pixels.push(bg.2);
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    #[inline]
    fn set_pixel(&mut self, x: usize, y: usize, color: (u8, u8, u8)) {
        if x < self.width && y < self.height {
            let idx = (y * self.width + x) * 3;
            self.pixels[idx] = color.0;
            self.pixels[idx + 1] = color.1;
            self.pixels[idx + 2] = color.2;
        }
    }

    fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: (u8, u8, u8)) {
        let x_end = (x + w).min(self.width);
        let y_end = (y + h).min(self.height);
        for py in y..y_end {
            for px in x..x_end {
                self.set_pixel(px, py, color);
            }
        }
    }

    fn stroke_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: (u8, u8, u8)) {
        if w == 0 || h == 0 {
            return;
        }
        self.fill_rect(x, y, w, 1, color);
        self.fill_rect(x, y + h - 1, w, 1, color);
        self.fill_rect(x, y, 1, h, color);
        self.fill_rect(x + w - 1, y, 1, h, color);
    }

    fn fill_circle(&mut self, cx: usize, cy: usize, r: usize, color: (u8, u8, u8)) {
        let r_i = r as isize;
        let r2 = r_i * r_i;
        for dy in -r_i..=r_i {
            for dx in -r_i..=r_i {
                if dx * dx + dy * dy <= r2 {
                    let px = cx as isize + dx;
                    let py = cy as isize + dy;
                    if px >= 0 && py >= 0 {
                        self.set_pixel(px as usize, py as usize, color);
                    }
                }
            }
        }
    }

    fn draw_text(&mut self, x: usize, y: usize, scale: usize, text: &str, color: (u8, u8, u8)) {
        let mut cursor_x = x;
        let advance = 6 * scale;
        for ch in text.chars() {
            let glyph = glyph_5x7(ch);
            for (row_idx, &row_bits) in glyph.iter().enumerate() {
                for col_idx in 0..5 {
                    if (row_bits >> (4 - col_idx)) & 1 == 1 {
                        self.fill_rect(
                            cursor_x + col_idx * scale,
                            y + row_idx * scale,
                            scale,
                            scale,
                            color,
                        );
                    }
                }
            }
            cursor_x += advance;
        }
    }
}

/// Encodes raw 8-bit RGB pixel rows into a standards-compliant PNG using uncompressed
/// zlib stored blocks + IEEE CRC32 + Adler32 (zero external dependencies).
pub fn encode_rgb_png(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let row_bytes = (width as usize) * 3;
    debug_assert_eq!(rgb.len(), row_bytes * (height as usize));

    let mut raw_scanlines = Vec::with_capacity((row_bytes + 1) * (height as usize));
    for y in 0..(height as usize) {
        raw_scanlines.push(0u8); // PNG filter type 0 (None)
        let start = y * row_bytes;
        raw_scanlines.extend_from_slice(&rgb[start..start + row_bytes]);
    }

    let compressed = zlib_store(&raw_scanlines);

    let mut out = Vec::with_capacity(compressed.len() + 64);
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");

    let mut ihdr = [0u8; 13];
    ihdr[0..4].copy_from_slice(&width.to_be_bytes());
    ihdr[4..8].copy_from_slice(&height.to_be_bytes());
    ihdr[8] = 8; // bit depth
    ihdr[9] = 2; // color type: Truecolor RGB
    ihdr[10] = 0; // compression method
    ihdr[11] = 0; // filter method
    ihdr[12] = 0; // interlace method
    write_png_chunk(&mut out, b"IHDR", &ihdr);
    write_png_chunk(&mut out, b"IDAT", &compressed);
    write_png_chunk(&mut out, b"IEND", &[]);
    out
}

fn write_png_chunk(out: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(chunk_type);
    out.extend_from_slice(data);
    let mut crc = 0xFFFF_FFFFu32;
    crc = crc32_update(crc, chunk_type);
    crc = crc32_update(crc, data);
    out.extend_from_slice(&(!crc).to_be_bytes());
}

fn crc32_update(mut crc: u32, bytes: &[u8]) -> u32 {
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}

fn zlib_store(raw: &[u8]) -> Vec<u8> {
    const MAX_BLOCK: usize = 65_535;
    let num_blocks = raw.len().div_ceil(MAX_BLOCK).max(1);
    let mut out = Vec::with_capacity(2 + raw.len() + num_blocks * 5 + 4);
    // RFC 1950 zlib header: CMF=0x78 (deflate, 32K window), FLG=0x01 (check bits: (0x7801 % 31) == 0).
    out.push(0x78);
    out.push(0x01);

    if raw.is_empty() {
        out.push(0x01); // final stored block
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0xFFFFu16.to_le_bytes());
    } else {
        let mut offset = 0;
        while offset < raw.len() {
            let end = (offset + MAX_BLOCK).min(raw.len());
            let chunk = &raw[offset..end];
            let is_final = end == raw.len();
            let len = chunk.len() as u16;
            let nlen = !len;
            out.push(if is_final { 0x01 } else { 0x00 });
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&nlen.to_le_bytes());
            out.extend_from_slice(chunk);
            offset = end;
        }
    }

    let adler = adler32(raw);
    out.extend_from_slice(&adler.to_be_bytes());
    out
}

fn adler32(bytes: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let mut s1: u32 = 1;
    let mut s2: u32 = 0;
    for &b in bytes {
        s1 = (s1 + b as u32) % MOD;
        s2 = (s2 + s1) % MOD;
    }
    (s2 << 16) | s1
}

/// Crisp built-in 5x7 monospace bitmap font for printable ASCII (32..=126).
/// Each byte encodes 5 horizontal pixels in bits 4..=0 (MSB on the left).
fn glyph_5x7(ch: char) -> [u8; 7] {
    match ch {
        ' ' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        '!' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x00, 0x04],
        '"' => [0x0A, 0x0A, 0x0A, 0x00, 0x00, 0x00, 0x00],
        '#' => [0x0A, 0x0A, 0x1F, 0x0A, 0x1F, 0x0A, 0x0A],
        '$' => [0x04, 0x0F, 0x14, 0x0E, 0x05, 0x1E, 0x04],
        '%' => [0x18, 0x19, 0x02, 0x04, 0x08, 0x13, 0x03],
        '&' => [0x0C, 0x12, 0x14, 0x08, 0x15, 0x12, 0x0D],
        '\'' => [0x04, 0x04, 0x08, 0x00, 0x00, 0x00, 0x00],
        '(' => [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02],
        ')' => [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08],
        '*' => [0x00, 0x04, 0x15, 0x0E, 0x15, 0x04, 0x00],
        '+' => [0x00, 0x04, 0x04, 0x1F, 0x04, 0x04, 0x00],
        ',' => [0x00, 0x00, 0x00, 0x00, 0x04, 0x04, 0x08],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        '/' => [0x01, 0x02, 0x02, 0x04, 0x08, 0x08, 0x10],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        ':' => [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00],
        ';' => [0x00, 0x0C, 0x0C, 0x00, 0x04, 0x04, 0x08],
        '<' => [0x02, 0x04, 0x08, 0x10, 0x08, 0x04, 0x02],
        '=' => [0x00, 0x00, 0x1F, 0x00, 0x1F, 0x00, 0x00],
        '>' => [0x08, 0x04, 0x02, 0x01, 0x02, 0x04, 0x08],
        '?' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04],
        '@' => [0x0E, 0x11, 0x17, 0x15, 0x17, 0x10, 0x0E],
        'A' => [0x04, 0x0A, 0x11, 0x11, 0x1F, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1C, 0x12, 0x11, 0x11, 0x11, 0x12, 0x1C],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x1B, 0x11],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '[' => [0x0E, 0x08, 0x08, 0x08, 0x08, 0x08, 0x0E],
        '\\' => [0x10, 0x08, 0x08, 0x04, 0x02, 0x02, 0x01],
        ']' => [0x0E, 0x02, 0x02, 0x02, 0x02, 0x02, 0x0E],
        '^' => [0x04, 0x0A, 0x11, 0x00, 0x00, 0x00, 0x00],
        '_' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1F],
        '`' => [0x08, 0x04, 0x02, 0x00, 0x00, 0x00, 0x00],
        'a' => [0x00, 0x00, 0x0E, 0x01, 0x0F, 0x11, 0x0F],
        'b' => [0x10, 0x10, 0x1E, 0x11, 0x11, 0x11, 0x1E],
        'c' => [0x00, 0x00, 0x0E, 0x10, 0x10, 0x11, 0x0E],
        'd' => [0x01, 0x01, 0x0F, 0x11, 0x11, 0x11, 0x0F],
        'e' => [0x00, 0x00, 0x0E, 0x11, 0x1F, 0x10, 0x0E],
        'f' => [0x06, 0x08, 0x1E, 0x08, 0x08, 0x08, 0x08],
        'g' => [0x00, 0x0F, 0x11, 0x11, 0x0F, 0x01, 0x0E],
        'h' => [0x10, 0x10, 0x1E, 0x11, 0x11, 0x11, 0x11],
        'i' => [0x04, 0x00, 0x0C, 0x04, 0x04, 0x04, 0x0E],
        'j' => [0x02, 0x00, 0x06, 0x02, 0x02, 0x12, 0x0C],
        'k' => [0x10, 0x10, 0x12, 0x14, 0x18, 0x14, 0x12],
        'l' => [0x0C, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'm' => [0x00, 0x00, 0x1A, 0x15, 0x15, 0x11, 0x11],
        'n' => [0x00, 0x00, 0x1E, 0x11, 0x11, 0x11, 0x11],
        'o' => [0x00, 0x00, 0x0E, 0x11, 0x11, 0x11, 0x0E],
        'p' => [0x00, 0x00, 0x1E, 0x11, 0x1E, 0x10, 0x10],
        'q' => [0x00, 0x00, 0x0F, 0x11, 0x0F, 0x01, 0x01],
        'r' => [0x00, 0x00, 0x16, 0x19, 0x10, 0x10, 0x10],
        's' => [0x00, 0x00, 0x0F, 0x10, 0x0E, 0x01, 0x1E],
        't' => [0x08, 0x08, 0x1E, 0x08, 0x08, 0x09, 0x06],
        'u' => [0x00, 0x00, 0x11, 0x11, 0x11, 0x13, 0x0D],
        'v' => [0x00, 0x00, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'w' => [0x00, 0x00, 0x11, 0x11, 0x15, 0x15, 0x0A],
        'x' => [0x00, 0x00, 0x11, 0x0A, 0x04, 0x0A, 0x11],
        'y' => [0x00, 0x00, 0x11, 0x11, 0x0F, 0x01, 0x0E],
        'z' => [0x00, 0x00, 0x1F, 0x02, 0x04, 0x08, 0x1F],
        '{' => [0x02, 0x04, 0x04, 0x08, 0x04, 0x04, 0x02],
        '|' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        '}' => [0x08, 0x04, 0x04, 0x02, 0x04, 0x04, 0x08],
        '~' => [0x00, 0x08, 0x15, 0x02, 0x00, 0x00, 0x00],
        _ => [0x1F, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1F],
    }
}
