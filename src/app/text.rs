//! UTF-8 editing limits match the byte limits of the persisted schema.
use eframe::egui::TextBuffer;

pub(super) struct ByteText<'a> {
    pub text: &'a mut String,
    pub limit: usize,
}

impl TextBuffer for ByteText<'_> {
    fn is_mutable(&self) -> bool {
        true
    }
    fn as_str(&self) -> &str {
        self.text
    }
    fn insert_text(&mut self, input: &str, char_index: usize) -> usize {
        let mut end = input.len().min(self.limit.saturating_sub(self.text.len()));
        while !input.is_char_boundary(end) {
            end -= 1;
        }
        <String as TextBuffer>::insert_text(self.text, &input[..end], char_index)
    }
    fn delete_char_range(&mut self, range: std::ops::Range<usize>) {
        <String as TextBuffer>::delete_char_range(self.text, range);
    }
    fn clear(&mut self) {
        self.text.clear();
    }
    fn type_id(&self) -> std::any::TypeId {
        std::any::TypeId::of::<ByteText<'static>>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_insert_delete_and_replace_use_char_indices_and_byte_budget() {
        let mut text = "AéZ".to_owned();
        let mut buffer = ByteText {
            text: &mut text,
            limit: 10,
        };
        assert_eq!(buffer.insert_text("🌈東京", 2), 1);
        assert_eq!(buffer.as_str(), "Aé🌈Z");
        buffer.delete_char_range(1..3);
        assert_eq!(buffer.as_str(), "AZ");
        assert_eq!(buffer.insert_text("東京🌈", 1), 2);
        assert_eq!(buffer.as_str(), "A東京Z");
        buffer.replace_with("🌈🌈🌈");
        assert_eq!(buffer.as_str(), "🌈🌈");
        assert_eq!(buffer.insert_text("é", 1), 1);
        assert_eq!(buffer.as_str(), "🌈é🌈");
        assert_eq!(buffer.insert_text("x", 1), 0);
    }
    #[test]
    fn rendering_an_oversized_existing_draft_never_truncates_it() {
        let mut text = "東京".repeat(10);
        let original = text.clone();
        let mut buffer = ByteText {
            text: &mut text,
            limit: 12,
        };
        assert_eq!(buffer.as_str(), original);
        assert_eq!(buffer.insert_text("x", 0), 0);
        buffer.delete_char_range(0..18);
        assert_eq!(buffer.as_str(), "東京");
        assert_eq!(buffer.insert_text("東京", 2), 2);
        assert_eq!(buffer.as_str().len(), 12);
    }
}
