//! Colores ANSI de kuaky (16 base + cubo 6×6×6 + grises).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn hex(v: u32) -> Self {
        Self { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8 }
    }

    pub fn dim(self) -> Self {
        Self {
            r: (self.r as f32 * 0.66) as u8,
            g: (self.g as f32 * 0.66) as u8,
            b: (self.b as f32 * 0.66) as u8,
        }
    }
}

pub const FOREGROUND: Rgb = Rgb::hex(0xd8dbe2);
pub const DIM_FOREGROUND: Rgb = Rgb::hex(0x8a909c);
pub const BACKGROUND: Rgb = Rgb::hex(0x0b0c0f);
pub const CURSOR: Rgb = Rgb::hex(0x5eead4);
pub const SELECTION: Rgb = Rgb::hex(0x264f4a);

const BASE: [u32; 16] = [
    0x1c1f26, // negro
    0xf87171, // rojo
    0x4ade80, // verde
    0xfacc15, // amarillo
    0x60a5fa, // azul
    0xc084fc, // magenta
    0x5eead4, // cian
    0xd8dbe2, // blanco
    0x555b66, // negro brillante
    0xfca5a5,
    0x86efac,
    0xfde047,
    0x93c5fd,
    0xd8b4fe,
    0x99f6e4,
    0xffffff,
];

pub fn color_for_index(i: usize) -> Rgb {
    match i {
        0..=15 => Rgb::hex(BASE[i]),
        16..=231 => {
            let i = i - 16;
            let level = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
            Rgb { r: level(i / 36), g: level((i / 6) % 6), b: level(i % 6) }
        }
        232..=255 => {
            let v = (8 + (i - 232) * 10) as u8;
            Rgb { r: v, g: v, b: v }
        }
        _ => FOREGROUND,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xterm_256_table() {
        assert_eq!(color_for_index(16), Rgb::hex(0x000000));
        assert_eq!(color_for_index(231), Rgb::hex(0xffffff));
        assert_eq!(color_for_index(196), Rgb::hex(0xff0000));
        assert_eq!(color_for_index(232), Rgb::hex(0x080808));
    }
}
