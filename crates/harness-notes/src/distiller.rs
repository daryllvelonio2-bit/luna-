pub struct PointerDistiller;

impl PointerDistiller {
    /// Distills a note's title and content into a strict 1 to 5 word memory pointer.
    pub fn distill(title: &str, content: &str) -> String {
        let clean_title = title
            .chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace() || *c == '-')
            .collect::<String>();

        let words: Vec<&str> = clean_title
            .split_whitespace()
            .filter(|w| !w.is_empty())
            .collect();

        if !words.is_empty() {
            let pointer_words = &words[..words.len().min(5)];
            return pointer_words.join(" ");
        }

        // Fallback to first line or first 5 words of content
        let clean_content = content
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace())
            .collect::<String>();

        let content_words: Vec<&str> = clean_content
            .split_whitespace()
            .filter(|w| !w.is_empty())
            .collect();

        if !content_words.is_empty() {
            let pointer_words = &content_words[..content_words.len().min(5)];
            pointer_words.join(" ")
        } else {
            "Untitled Document".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strict_distillation() {
        let p1 = PointerDistiller::distill("Stripe Payment Gateway Spec 2026", "details...");
        assert_eq!(p1, "Stripe Payment Gateway Spec 2026");

        let p2 = PointerDistiller::distill(
            "A Very Long Title That Definitely Exceeds Five Individual Words In Length",
            "details...",
        );
        let count = p2.split_whitespace().count();
        assert!(count <= 5);
        assert_eq!(p2, "A Very Long Title That");
    }
}
