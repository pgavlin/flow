//! Compact line-breaking items with paragraph-local, independently stored atoms.
use std::ops::{Deref, DerefMut};

use text_layout::Item;

use super::{Atom, LayoutItem, TextStyle};
use crate::model::flow::{BaseDirection, Destination};

/// An index into one paragraph's atom storage, not into its item sequence.
/// Inserting, removing, or draining items never changes other atom IDs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AtomId(usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct StyleId(usize);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LinkId(usize);

struct ParagraphStyle {
    value: TextStyle,
    directions: [Option<StyleId>; 3],
}

#[derive(Default)]
pub(super) struct ParagraphItems<'text> {
    items: Vec<LayoutItem>,
    // Taking an initial-letter atom leaves a hole instead of moving atoms and
    // invalidating IDs. All slots, including unused ones, are cleared together.
    atoms: Vec<Option<Atom<'text>>>,
    styles: Vec<ParagraphStyle>,
    links: Vec<Destination>,
}

impl<'text> ParagraphItems<'text> {
    // Share styles at source-span boundaries, not by hashing every word.
    // Appending is linear even when every span has a distinct author style.
    pub(super) fn push_style(&mut self, value: TextStyle) -> StyleId {
        let id = StyleId(self.styles.len());
        self.styles.push(ParagraphStyle {
            value,
            directions: [None; 3],
        });
        id
    }

    pub(super) fn style(&self, id: StyleId) -> &TextStyle {
        &self.styles[id.0].value
    }

    pub(super) fn directional_style(&mut self, id: StyleId, direction: BaseDirection) -> StyleId {
        if self.style(id).shape_direction == direction {
            return id;
        }
        let index = match direction {
            BaseDirection::Auto => 0,
            BaseDirection::LeftToRight => 1,
            BaseDirection::RightToLeft => 2,
        };
        if let Some(cached) = self.styles[id.0].directions[index] {
            return cached;
        }
        let mut style = self.style(id).clone();
        style.shape_direction = direction;
        let derived = self.push_style(style);
        self.styles[id.0].directions[index] = Some(derived);
        derived
    }

    pub(super) fn push_link(&mut self, link: Destination) -> LinkId {
        let id = LinkId(self.links.len());
        self.links.push(link);
        id
    }

    pub(super) fn link(&self, id: LinkId) -> &Destination {
        &self.links[id.0]
    }
    pub(super) fn push_atom(&mut self, atom: Atom<'text>) -> AtomId {
        let id = AtomId(self.atoms.len());
        self.atoms.push(Some(atom));
        id
    }

    pub(super) fn atom(&self, id: AtomId) -> &Atom<'text> {
        self.atoms[id.0].as_ref().expect("item retains a live atom")
    }

    pub(super) fn atom_mut(&mut self, id: AtomId) -> &mut Atom<'text> {
        self.atoms[id.0].as_mut().expect("item retains a live atom")
    }

    pub(super) fn take_atom(&mut self, id: AtomId) -> Atom<'text> {
        self.atoms[id.0].take().expect("item retains a live atom")
    }

    pub(super) fn push_box(&mut self, atom: Atom<'text>) {
        let width = atom.metrics.width;
        let data = self.push_atom(atom);
        self.items.push(Item::Box { width, data });
    }

    pub(super) fn insert_box(&mut self, index: usize, atom: Atom<'text>) {
        let width = atom.metrics.width;
        let data = self.push_atom(atom);
        self.items.insert(index, Item::Box { width, data });
    }

    pub(super) fn replace_box(&mut self, index: usize, atom: Atom<'text>) {
        let Item::Box { data, .. } = self.items[index] else {
            panic!("only box items can be replaced with an atom");
        };
        let width = atom.metrics.width;
        self.atoms[data.0] = Some(atom);
        self.items[index] = Item::Box { width, data };
    }

    pub(super) fn is_recycled(&self) -> bool {
        self.items.is_empty()
            && self.atoms.is_empty()
            && self.styles.is_empty()
            && self.links.is_empty()
    }

    pub(super) fn capacity_bytes(&self) -> usize {
        self.items.capacity() * std::mem::size_of::<LayoutItem>()
            + self.atoms.capacity() * std::mem::size_of::<Option<Atom<'text>>>()
            + self.styles.capacity() * std::mem::size_of::<ParagraphStyle>()
            + self.links.capacity() * std::mem::size_of::<Destination>()
    }

    pub(super) fn clear_bounded(&mut self, max_bytes: usize) {
        self.items.clear();
        // Clear source borrows and drop generated text and shared owned data.
        self.atoms.clear();
        self.styles.clear();
        self.links.clear();
        // All backing allocations count against the existing scratch budget.
        if self.capacity_bytes() > max_bytes {
            *self = Self::default();
        }
    }
}

// Internal callers can edit the item sequence, but atom storage is append-only
// until the paragraph is recycled. Creating boxes/hyphens requires an AtomId.
impl Deref for ParagraphItems<'_> {
    type Target = Vec<LayoutItem>;

    fn deref(&self) -> &Self::Target {
        &self.items
    }
}

impl DerefMut for ParagraphItems<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.items
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        layout::{Break, FontFamily, FontWeight, TextMetrics, TextStyle, set_flow_link},
        model::{
            TextRange,
            flow::{BaseDirection, BidiState, Destination},
        },
    };

    fn style() -> TextStyle {
        TextStyle {
            family: FontFamily::Named("Fixture family".into()),
            size: 12.0,
            weight: FontWeight::Normal,
            italic: false,
            strikethrough: false,
            link: false,
            initial_letter: None,
            baseline_shift: 0.0,
            letter_spacing: 0.0,
            bidi: BidiState::default(),
            bidi_boundary: None,
            shape_direction: BaseDirection::LeftToRight,
        }
    }

    fn paragraph<'text>() -> ParagraphItems<'text> {
        let mut items = ParagraphItems::default();
        assert_eq!(items.push_style(style()), StyleId(0));
        items
    }

    fn atom(text: &str) -> Atom<'_> {
        Atom {
            text: text.into(),
            range: TextRange {
                start: 0,
                end: text.len(),
            },
            style: StyleId(0),
            flow_link: None,
            metrics: TextMetrics {
                width: text.len() as f32,
                ascent: 10.0,
                descent: 2.0,
            },
            atomic: false,
        }
    }

    #[test]
    fn breaking_items_do_not_embed_owned_atoms() {
        assert!(std::mem::size_of::<LayoutItem>() <= 6 * std::mem::size_of::<usize>());
        assert!(std::mem::size_of::<Break>() <= 2 * std::mem::size_of::<usize>());
    }

    #[test]
    fn atom_ids_survive_item_edits_and_atom_storage_growth() {
        let mut items = paragraph();
        items.push_box(atom("initial letter"));
        let Item::Box { data: initial, .. } = items[0] else {
            unreachable!()
        };
        let hyphen = items.push_atom(atom("-"));
        items.push(Item::Penalty {
            width: 0.0,
            cost: 10.0,
            flagged: true,
            data: Break::Hyphen(hyphen),
        });
        for _ in 0..128 {
            items.push_box(atom("word"));
        }
        let Item::Box { data: last, .. } = *items.last().unwrap() else {
            unreachable!()
        };
        items.insert_box(0, atom("indent"));
        items.remove(0);
        // Remove the cap's item, then move its owned atom into output. Taking
        // the atom must not shift any later IDs (including the hyphen).
        items.remove(0);
        let cap = items.take_atom(initial);
        assert_eq!(cap.text, "initial letter");
        items.drain(1..10);
        assert_eq!(items.atom(hyphen).text, "-");
        assert_eq!(items.atom(last).text, "word");
        let index = items.len() - 1;
        items.replace_box(index, atom("replacement"));
        assert_eq!(items.atom(last).text, "replacement");
        for item in items.iter() {
            match item {
                Item::Box { data, .. }
                | Item::Penalty {
                    data: Break::Hyphen(data),
                    ..
                } => {
                    assert!(!items.atom(*data).text.is_empty());
                }
                _ => (),
            }
        }
        let next = items.push_atom(atom("new"));
        assert_ne!(next, initial);
        items.clear_bounded(usize::MAX);
        assert!(items.is_recycled());
        assert_eq!(cap.text, "initial letter");
    }

    #[test]
    fn link_assignment_resolves_both_boxes_and_discretionary_hyphens() {
        let mut items = paragraph();
        items.push_box(atom("prefix"));
        let start = items.len();
        items.push_box(atom("linked"));
        let hyphen = items.push_atom(atom("-"));
        items.push(Item::Penalty {
            width: 0.0,
            cost: 10.0,
            flagged: true,
            data: Break::Hyphen(hyphen),
        });
        let destination = Destination::External("https://example.invalid/".into());
        set_flow_link(&mut items, start, Some(&destination));
        assert_eq!(items.links.len(), 1);
        let Item::Box { data: prefix, .. } = items[0] else {
            unreachable!()
        };
        let Item::Box { data: linked, .. } = items[1] else {
            unreachable!()
        };
        assert!(items.atom(prefix).flow_link.is_none());
        assert_eq!(
            items.atom(linked).flow_link.map(|link| items.link(link)),
            Some(&destination)
        );
        assert_eq!(
            items.atom(hyphen).flow_link.map(|link| items.link(link)),
            Some(&destination)
        );
    }

    struct FixedMeasurer;

    impl crate::layout::TextMeasurer for FixedMeasurer {
        fn measure(&self, text: &str, style: &TextStyle) -> TextMetrics {
            TextMetrics {
                width: text.chars().count() as f32 * style.size * 0.5,
                ascent: style.size * 0.8,
                descent: style.size * 0.2,
            }
        }
    }

    #[test]
    fn source_words_borrow_text_and_share_one_span_style() {
        use crate::layout::{LayoutOptions, WordBreaking, push_text_items};
        let source = String::from("alpha beta gamma delta");
        let mut items = ParagraphItems::default();
        push_text_items(
            &mut items,
            &source,
            TextRange {
                start: 0,
                end: source.len(),
            },
            style(),
            &FixedMeasurer,
            WordBreaking::Normal,
            &LayoutOptions::default(),
        );
        let mut words = 0;
        for item in items.iter() {
            if let Item::Box { data, .. } = item {
                let atom = items.atom(*data);
                assert!(matches!(atom.text, std::borrow::Cow::Borrowed(_)));
                assert!(atom.text.as_ptr() >= source.as_ptr());
                assert!(
                    (atom.text.as_ptr() as usize + atom.text.len())
                        <= source.as_ptr() as usize + source.len()
                );
                assert_eq!(atom.style, StyleId(0));
                words += 1;
            }
        }
        assert_eq!(words, 4);
        assert_eq!(items.styles.len(), 1);
    }

    #[test]
    fn directional_styles_are_shared_without_mutating_the_base_style() {
        let mut items = ParagraphItems::default();
        let mut value = style();
        value.shape_direction = BaseDirection::Auto;
        let base = items.push_style(value.clone());
        let ltr = items.directional_style(base, BaseDirection::LeftToRight);
        let rtl = items.directional_style(base, BaseDirection::RightToLeft);
        for _ in 0..100 {
            assert_eq!(
                items.directional_style(base, BaseDirection::LeftToRight),
                ltr
            );
            assert_eq!(
                items.directional_style(base, BaseDirection::RightToLeft),
                rtl
            );
        }
        assert_eq!(items.styles.len(), 3);
        assert_eq!(items.style(base), &value);
        assert_eq!(items.style(ltr).shape_direction, BaseDirection::LeftToRight);
        assert_eq!(items.style(rtl).shape_direction, BaseDirection::RightToLeft);
        let mut different = value;
        different.italic = true;
        let other = items.push_style(different);
        let other_ltr = items.directional_style(other, BaseDirection::LeftToRight);
        assert_ne!(other_ltr, ltr);
        assert!(items.style(other_ltr).italic);
        assert!(!items.style(ltr).italic);
    }

    #[test]
    fn generated_text_slices_own_their_backing() {
        let slice = {
            let mut generated = atom("");
            generated.text = std::borrow::Cow::Owned("“Charlie".into());
            generated.text_slice("“C".len()..generated.text.len())
        };
        assert!(matches!(slice, std::borrow::Cow::Owned(_)));
        assert_eq!(slice, "harlie");
    }

    #[test]
    fn final_runs_outlive_borrowed_source_and_shared_styles_and_links() {
        use crate::layout::{
            Break, LayoutOptions, WordBreaking, line_breaks, materialize_lines, push_text_items,
            terminate,
        };
        let lines = {
            let source = String::from("alpha beta gamma delta");
            let mut items = ParagraphItems::default();
            let options = LayoutOptions::default();
            push_text_items(
                &mut items,
                &source,
                TextRange {
                    start: 0,
                    end: source.len(),
                },
                style(),
                &FixedMeasurer,
                WordBreaking::Normal,
                &options,
            );
            let link = Destination::External(String::from("https://example.invalid/"));
            set_flow_link(&mut items, 0, Some(&link));
            terminate(&mut items, Break::Paragraph);
            let breaks = line_breaks(&items, options.width);
            let lines = materialize_lines(&items, breaks, 0.0, 0.0, &options, false);
            items.clear_bounded(usize::MAX);
            assert!(items.is_recycled());
            lines
        };
        let runs = lines.iter().flat_map(|line| &line.runs).collect::<Vec<_>>();
        assert_eq!(
            runs.iter().map(|run| run.text.as_str()).collect::<Vec<_>>(),
            ["alpha", "beta", "gamma", "delta"]
        );
        for run in runs {
            assert_eq!(run.style.family, FontFamily::Named("Fixture family".into()));
            assert_eq!(
                run.flow_link,
                Some(Destination::External("https://example.invalid/".into()))
            );
        }
    }

    #[test]
    fn style_and_link_capacity_count_against_the_same_scratch_budget() {
        let mut items = paragraph();
        for _ in 0..32 {
            items.push_style(style());
            items.push_link(Destination::External("https://example.invalid/".into()));
        }
        let expected = items.styles.capacity() * std::mem::size_of::<ParagraphStyle>()
            + items.links.capacity() * std::mem::size_of::<Destination>();
        assert_eq!(items.capacity_bytes(), expected);
        items.clear_bounded(expected - 1);
        assert!(items.is_recycled());
        assert_eq!(items.capacity_bytes(), 0);
    }

    #[test]
    fn recycling_reuses_both_buffers_and_budgets_them_together() {
        let mut items = paragraph();
        items.push_box(atom("word"));
        let before = (items.items.as_ptr(), items.atoms.as_ptr());
        let bytes = items.capacity_bytes();
        assert!(bytes > items.items.capacity() * std::mem::size_of::<LayoutItem>());
        items.clear_bounded(bytes);
        assert!(items.is_recycled());
        items.push_style(style());
        items.push_box(atom("next"));
        assert_eq!((items.items.as_ptr(), items.atoms.as_ptr()), before);
        items.clear_bounded(bytes - 1);
        assert_eq!(items.capacity_bytes(), 0);
        assert!(items.is_recycled());
    }
}
