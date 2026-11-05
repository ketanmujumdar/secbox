//! The sixteen colours a text terminal of the period actually had.
//!
//! Each style is a foreground and background pair, written as the escape
//! codes a terminal understands. Keeping them in one place means the layout
//! never writes an escape sequence itself, so a width calculation cannot
//! count one by mistake.

/// A colour pair. The names are roles, not colours, so the palette can change
/// in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// The patterned background behind every window.
    Desktop,
    MenuBar,
    /// The letter that chooses a menu entry.
    MenuHot,
    MenuActive,
    /// Inside a window.
    Window,
    WindowDim,
    WindowGood,
    WindowWarn,
    WindowAlert,
    Frame,
    Title,
    /// The highlighted row of a list.
    Selected,
    /// The strip of shadow a window casts.
    Shadow,
    /// A dialog box, which is grey rather than blue.
    Dialog,
    DialogHot,
    DialogFrame,
    Button,
    ButtonHot,
    /// The bar along the bottom: the number, then the word.
    StatusKey,
    StatusLabel,
    Scrollbar,
    ScrollThumb,
}

impl Style {
    /// The escape sequence that turns this style on.
    pub fn codes(self) -> &'static str {
        match self {
            // Grey pattern on blue, the background of every one of these.
            Style::Desktop => "90;44",
            Style::MenuBar => "30;47",
            Style::MenuHot => "31;47",
            Style::MenuActive => "30;46",
            Style::Window => "37;44",
            Style::WindowDim => "36;44",
            Style::WindowGood => "92;44",
            Style::WindowWarn => "93;44",
            Style::WindowAlert => "97;41",
            Style::Frame => "37;44",
            Style::Title => "93;44",
            Style::Selected => "30;46",
            Style::Shadow => "30;40",
            Style::Dialog => "30;47",
            Style::DialogHot => "31;47",
            Style::DialogFrame => "30;47",
            Style::Button => "97;42",
            Style::ButtonHot => "93;42",
            Style::StatusKey => "97;40",
            Style::StatusLabel => "30;46",
            Style::Scrollbar => "34;44",
            Style::ScrollThumb => "97;44",
        }
    }
}

/// The same colour from the fixed part of the 256-colour table. Terminal themes
/// recolour the first sixteen entries, which can turn the blue purple, and
/// leave these alone.
fn fixed_index(code: u16) -> Option<u16> {
    Some(match code {
        30 => 16,
        31 => 124,
        32 => 34,
        33 => 142,
        34 => 19,
        35 => 127,
        36 => 37,
        37 => 250,
        90 => 244,
        91 => 196,
        92 => 46,
        93 => 226,
        94 => 21,
        95 => 201,
        96 => 51,
        97 => 231,
        _ => return None,
    })
}

impl Style {
    /// The escape parameters for this style, on a terminal that does or does
    /// not have the 256-colour table.
    pub fn sequence(self, fixed: bool) -> String {
        let codes = self.codes();
        if !fixed {
            return codes.to_owned();
        }
        let converted = codes.split_once(';').and_then(|(fg, bg)| {
            let fg = fixed_index(fg.parse().ok()?)?;
            let bg = fixed_index(bg.parse::<u16>().ok()?.checked_sub(10)?)?;
            Some(format!("38;5;{fg};48;5;{bg}"))
        });
        converted.unwrap_or_else(|| codes.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::Style;

    #[test]
    fn the_window_is_the_same_blue_whatever_the_theme_says() {
        assert_eq!(Style::Window.sequence(true), "38;5;250;48;5;19");
        assert_eq!(Style::Window.sequence(false), "37;44");
    }

    /// Every style has to be one of the pairs a text terminal of the time
    /// could show: one foreground, one background, nothing else.
    #[test]
    fn every_style_is_a_plain_colour_pair() {
        for style in [
            Style::Desktop,
            Style::MenuBar,
            Style::MenuHot,
            Style::MenuActive,
            Style::Window,
            Style::WindowDim,
            Style::WindowGood,
            Style::WindowWarn,
            Style::WindowAlert,
            Style::Frame,
            Style::Title,
            Style::Selected,
            Style::Shadow,
            Style::Dialog,
            Style::DialogHot,
            Style::DialogFrame,
            Style::Button,
            Style::ButtonHot,
            Style::StatusKey,
            Style::StatusLabel,
            Style::Scrollbar,
            Style::ScrollThumb,
        ] {
            let codes = style.codes();
            let parts: Vec<&str> = codes.split(';').collect();
            assert_eq!(parts.len(), 2, "{style:?} is {codes}");
            let fg: u16 = parts[0].parse().expect("a number");
            let bg: u16 = parts[1].parse().expect("a number");
            assert!(
                (30..=37).contains(&fg) || (90..=97).contains(&fg),
                "{style:?} foreground {fg}"
            );
            assert!(
                (40..=47).contains(&bg) || (100..=107).contains(&bg),
                "{style:?} background {bg}"
            );
        }
    }
}
