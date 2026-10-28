//! Sends only what changed: the app describes its whole interface every
//! time, and the view compares that with what the renderer already has.
//! Elements that are sent again with the same id are replaced in place, so
//! the renderer keeps their children and widget state (focus, the open tab).

use hypereffect::{Element, Outbox};
use std::collections::{HashMap, HashSet};

/// The elements of one rendering, parents before their children.
#[derive(Default)]
pub struct Tree(Vec<Element>);

impl Tree {
    pub fn element(&mut self, e: Element) {
        self.0.push(e);
    }
}

/// What the renderer has been sent.
#[derive(Default)]
pub struct View {
    /// By id, without `before`.
    shown: HashMap<String, Element>,
    /// The ids of each parent's children ("" for the root), in order.
    children: HashMap<String, Vec<String>>,
}

impl View {
    /// Sends what it takes to turn what the renderer has into `tree`.
    pub fn sync(&mut self, out: &mut Outbox, tree: Tree) {
        let wanted: HashMap<&str, &Element> = tree.0.iter().map(|e| (e.id.as_str(), e)).collect();
        // Gone, or of another type or parent: replacing those would drop
        // their children or move them, so they are removed and sent anew.
        let gone: HashSet<String> = self
            .shown
            .values()
            .filter(|old| match wanted.get(old.id.as_str()) {
                None => true,
                Some(new) => new.component != old.component || new.parent != old.parent,
            })
            .map(|e| e.id.clone())
            .collect();
        for id in &gone {
            // Removing an ancestor removes it already.
            if !self.ancestors(id).any(|a| gone.contains(a)) {
                out.remove(id.as_str());
            }
        }
        for id in &gone {
            self.forget(id);
        }

        // Each parent's remaining children are in their old order. Walking
        // the new tree in order, the first `next` of them already match it.
        let mut next: HashMap<String, usize> = HashMap::new();
        for e in tree.0 {
            let parent = e.parent.clone().unwrap_or_default();
            let i = next.entry(parent.clone()).or_default();
            let siblings = self.children.entry(parent).or_default();
            let send = match siblings.iter().position(|c| *c == e.id) {
                Some(at) if at == *i => (self.shown.get(&e.id) != Some(&e)).then_some(None),
                at => {
                    // New, or further down: it goes before what is at `i`.
                    if let Some(at) = at {
                        siblings.remove(at);
                    }
                    let before = siblings.get(*i).cloned();
                    siblings.insert(*i, e.id.clone());
                    Some(before)
                }
            };
            *i += 1;
            if let Some(before) = send {
                out.element(Element {
                    before,
                    ..e.clone()
                });
            }
            self.shown.insert(e.id.clone(), e);
        }
    }

    fn ancestors<'a>(&'a self, id: &str) -> impl Iterator<Item = &'a str> {
        let mut at = self.shown.get(id).and_then(|e| e.parent.as_deref());
        std::iter::from_fn(move || {
            let id = at?;
            at = self.shown.get(id).and_then(|e| e.parent.as_deref());
            Some(id)
        })
    }

    /// Drops an element and its descendants.
    fn forget(&mut self, id: &str) {
        let Some(e) = self.shown.remove(id) else {
            return;
        };
        if let Some(siblings) = self.children.get_mut(e.parent.as_deref().unwrap_or("")) {
            siblings.retain(|c| c != id);
        }
        for child in self.children.remove(id).unwrap_or_default() {
            self.forget(&child);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hypereffect::ui::*;

    fn sync(v: &mut View, els: Vec<Element>) -> Vec<String> {
        let mut out = Outbox::new();
        v.sync(&mut out, Tree(els));
        out.take().lines().map(String::from).collect()
    }

    #[test]
    fn sends_only_changes() {
        let mut v = View::default();
        let first = vec![
            column("c"),
            text("a").parent("c").text("A"),
            text("b").parent("c").text("B"),
        ];
        assert_eq!(sync(&mut v, first.clone()).len(), 3);
        assert!(sync(&mut v, first).is_empty());

        // Changed, inserted at the front, and removed.
        let lines = sync(
            &mut v,
            vec![
                column("c"),
                text("z").parent("c").text("Z"),
                text("a").parent("c").text("A2"),
            ],
        );
        assert_eq!(
            lines,
            vec![
                r#"remove {"id":"b"}"#,
                r#"element {"id":"z","type":"text","parent":"c","before":"a","text":"Z"}"#,
                r#"element {"id":"a","type":"text","parent":"c","text":"A2"}"#,
            ]
        );

        // Moved within its parent.
        let lines = sync(
            &mut v,
            vec![
                column("c"),
                text("a").parent("c").text("A2"),
                text("z").parent("c").text("Z"),
            ],
        );
        assert_eq!(
            lines,
            vec![r#"element {"id":"a","type":"text","parent":"c","before":"z","text":"A2"}"#]
        );
    }

    #[test]
    fn removing_a_parent_resends_kept_children() {
        let mut v = View::default();
        sync(&mut v, vec![column("c"), text("a").parent("c")]);
        let lines = sync(&mut v, vec![row("c"), text("a").parent("c")]);
        assert_eq!(
            lines,
            vec![
                r#"remove {"id":"c"}"#,
                r#"element {"id":"c","type":"row"}"#,
                r#"element {"id":"a","type":"text","parent":"c"}"#,
            ]
        );
    }
}
