#[derive(Default)]
pub struct Utf16Input(Option<u16>);
impl Utf16Input {
    pub fn clear(&mut self) {
        self.0 = None;
    }
    pub fn push(&mut self, unit: u16) -> Result<Option<char>, &'static str> {
        if (0xd800..=0xdbff).contains(&unit) {
            self.0 = Some(unit);
            return Ok(None);
        }
        let value = if (0xdc00..=0xdfff).contains(&unit) {
            let Some(high) = self.0.take() else {
                return Err("Получена неполная пара UTF-16");
            };
            0x10000 + ((u32::from(high) - 0xd800) << 10) + u32::from(unit) - 0xdc00
        } else {
            if self.0.take().is_some() {
                return Err("Получена неполная пара UTF-16");
            }
            u32::from(unit)
        };
        char::from_u32(value)
            .map(Some)
            .ok_or("Получен недопустимый символ Unicode")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supplementary_characters_are_committed_once_and_invalid_pairs_rejected() {
        let mut input = Utf16Input::default();
        assert_eq!(input.push(0xd83d).unwrap(), None);
        assert_eq!(input.push(0xde80).unwrap(), Some('🚀'));
        assert!(input.push(0xde80).is_err());
        assert_eq!(input.push('я' as u16).unwrap(), Some('я'));
        input.push(0xd83d).unwrap();
        input.clear();
        assert_eq!(input.push('x' as u16).unwrap(), Some('x'));
    }
}
