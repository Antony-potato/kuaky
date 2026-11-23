//! Traducción de teclas a las secuencias que espera un programa de terminal.

/// Tecla presionada, independiente del framework de interfaz.
#[derive(Debug, Clone, Default)]
pub struct Key<'a> {
    /// Nombre de la tecla: "a", "enter", "left", "f5", "space"…
    pub name: &'a str,
    /// Texto que produce la tecla con el teclado actual, si produce texto.
    pub text: Option<&'a str>,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// Bytes a enviar al PTY, o `None` si la tecla no aplica a la terminal.
pub fn key_to_bytes(key: &Key, app_cursor: bool) -> Option<Vec<u8>> {
    let csi = |final_char: char| -> Vec<u8> {
        let modifier = 1 + key.shift as u8 + 2 * key.alt as u8 + 4 * key.ctrl as u8;
        if modifier > 1 {
            format!("\x1b[1;{modifier}{final_char}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{final_char}").into_bytes()
        } else {
            format!("\x1b[{final_char}").into_bytes()
        }
    };
    let tilde = |code: u8| -> Vec<u8> {
        let modifier = 1 + key.shift as u8 + 2 * key.alt as u8 + 4 * key.ctrl as u8;
        if modifier > 1 {
            format!("\x1b[{code};{modifier}~").into_bytes()
        } else {
            format!("\x1b[{code}~").into_bytes()
        }
    };

    let bytes = match key.name {
        "enter" => {
            if key.shift || key.alt {
                // Salto de línea sin enviar (lo usan Claude Code y Codex).
                b"\x1b\r".to_vec()
            } else {
                b"\r".to_vec()
            }
        }
        "backspace" => {
            if key.alt || key.ctrl {
                b"\x17".to_vec() // borrar palabra
            } else {
                b"\x7f".to_vec()
            }
        }
        "tab" => {
            if key.shift {
                b"\x1b[Z".to_vec()
            } else {
                b"\t".to_vec()
            }
        }
        "escape" => b"\x1b".to_vec(),
        "up" => csi('A'),
        "down" => csi('B'),
        "right" => csi('C'),
        "left" => csi('D'),
        "home" => csi('H'),
        "end" => csi('F'),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "f1" => b"\x1bOP".to_vec(),
        "f2" => b"\x1bOQ".to_vec(),
        "f3" => b"\x1bOR".to_vec(),
        "f4" => b"\x1bOS".to_vec(),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        "space" if key.ctrl => vec![0],
        name if key.ctrl && name.chars().count() == 1 => {
            let c = name.chars().next()?.to_ascii_lowercase();
            let byte = match c {
                'a'..='z' => c as u8 - b'a' + 1,
                '@' | '2' => 0,
                '[' | '3' => 0x1b,
                '\\' | '4' => 0x1c,
                ']' | '5' => 0x1d,
                '^' | '6' => 0x1e,
                '_' | '-' | '7' => 0x1f,
                _ => return None,
            };
            if key.alt { vec![0x1b, byte] } else { vec![byte] }
        }
        _ => {
            let text = match key.text {
                Some(t) if !t.is_empty() => t.to_string(),
                _ if key.name == "space" => " ".to_string(),
                _ if key.name.chars().count() == 1 => key.name.to_string(),
                _ => return None,
            };
            let mut out = Vec::new();
            if key.alt {
                out.push(0x1b);
            }
            out.extend(text.into_bytes());
            out
        }
    };
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(name: &str) -> Key<'_> {
        Key { name, ..Key::default() }
    }

    #[test]
    fn translates_common_keys() {
        assert_eq!(key_to_bytes(&k("enter"), false).unwrap(), b"\r");
        assert_eq!(key_to_bytes(&k("up"), false).unwrap(), b"\x1b[A");
        assert_eq!(key_to_bytes(&k("up"), true).unwrap(), b"\x1bOA");
        assert_eq!(key_to_bytes(&Key { name: "c", ctrl: true, ..Key::default() }, false).unwrap(), [3]);
        assert_eq!(key_to_bytes(&Key { name: "a", text: Some("á"), ..Key::default() }, false).unwrap(), "á".as_bytes());
        assert_eq!(key_to_bytes(&Key { name: "enter", shift: true, ..Key::default() }, false).unwrap(), b"\x1b\r");
        assert_eq!(key_to_bytes(&Key { name: "right", ctrl: true, ..Key::default() }, false).unwrap(), b"\x1b[1;5C");
        assert!(key_to_bytes(&k("shift"), false).is_none());
    }
}
