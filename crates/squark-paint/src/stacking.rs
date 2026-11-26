// SPDX-License-Identifier: MIT
//! CSS 2.2 Appendix E and CSS Positioned Layout §2.2.

use crate::{PropertyRefs, input::*};

/// An element can contribute at several points in its context. In particular,
/// an in-flow block's decoration is below floats and its text is above them.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Phase {
    Decoration,
    Content,
    Outline,
}

#[derive(Debug)]
pub(crate) struct PaintFragment<'a> {
    pub fragment: &'a Fragment,
    pub decoration: PropertyRefs,
    pub content: PropertyRefs,
}

#[derive(Debug)]
enum Entry {
    Fragment(usize, Phase),
    Group(usize),
}

#[derive(Debug, Default)]
struct Context {
    root: usize,
    negative: Vec<(i32, usize)>,
    blocks: Vec<Entry>,
    floats: Vec<Entry>,
    inline: Vec<Entry>,
    positioned: Vec<Entry>,
    positive: Vec<(i32, usize)>,
    outlines: Vec<Entry>,
}

impl Context {
    fn emit(contexts: &[Self], index: usize, emit: &mut impl FnMut(usize, Phase)) {
        let context = &contexts[index];
        emit(context.root, Phase::Decoration);
        for &(_, index) in &context.negative {
            Self::emit(contexts, index, emit);
        }
        Self::emit_entries(contexts, &context.blocks, emit);
        Self::emit_entries(contexts, &context.floats, emit);
        emit(context.root, Phase::Content);
        Self::emit_entries(contexts, &context.inline, emit);
        Self::emit_entries(contexts, &context.positioned, emit);
        for &(_, index) in &context.positive {
            Self::emit(contexts, index, emit);
        }
        Self::emit_entries(contexts, &context.outlines, emit);
        emit(context.root, Phase::Outline);
    }

    fn emit_entries(contexts: &[Self], entries: &[Entry], emit: &mut impl FnMut(usize, Phase)) {
        for entry in entries {
            match entry {
                Entry::Fragment(index, phase) => emit(*index, *phase),
                Entry::Group(index) => Self::emit(contexts, *index, emit),
            }
        }
    }
}

/// The actual stacking context owns positioned and stacking descendants of
/// atomic in-flow groups such as floats and inline-blocks.
pub(crate) fn paint_order(
    fragments: &[PaintFragment<'_>],
    tree: &FragmentTree,
    mut emit: impl FnMut(usize, Phase),
) {
    if fragments.is_empty() {
        return;
    }

    let mut contexts = Vec::new();
    let mut top_layer = Vec::new();
    let mut pending = vec![(tree.root, 0, 0)];
    let mut index = 0;
    while let Some((id, stack, group)) = pending.pop() {
        let fragment = &tree.nodes[&id];
        let style = &fragment.style;
        if style.display == Display::None {
            continue;
        }
        debug_assert_eq!(fragments[index].fragment.id, id);
        let positioned = style.position != Position::Static;
        let level = if positioned || style.flex_or_grid_item {
            style.z_index
        } else {
            None
        };
        let (stack, group) = if id == tree.root || style.creates_context() || level.is_some() {
            let inner = contexts.len();
            contexts.push(Context {
                root: index,
                ..Context::default()
            });
            if id != tree.root {
                if let Some(order) = style.top_layer {
                    top_layer.push((order, inner));
                } else {
                    match level.unwrap_or(0) {
                        z if z < 0 => contexts[stack].negative.push((z, inner)),
                        0 => contexts[stack].positioned.push(Entry::Group(inner)),
                        z => contexts[stack].positive.push((z, inner)),
                    }
                }
            }
            (inner, inner)
        } else if positioned || style.floating || style.display == Display::InlineBlock {
            let inner = contexts.len();
            contexts.push(Context {
                root: index,
                ..Context::default()
            });
            let entry = Entry::Group(inner);
            if positioned {
                contexts[stack].positioned.push(entry);
            } else if style.floating {
                contexts[group].floats.push(entry);
            } else {
                contexts[group].inline.push(entry);
            }
            (stack, inner)
        } else {
            let context = &mut contexts[group];
            let decoration = Entry::Fragment(index, Phase::Decoration);
            if style.display == Display::Block
                && !matches!(
                    fragment.kind,
                    FragmentKind::Line | FragmentKind::Text | FragmentKind::Marker
                )
            {
                context.blocks.push(decoration);
            } else {
                context.inline.push(decoration);
            }
            context.inline.push(Entry::Fragment(index, Phase::Content));
            context
                .outlines
                .push(Entry::Fragment(index, Phase::Outline));
            (stack, group)
        };
        index += 1;
        pending.extend(
            fragment
                .children
                .iter()
                .rev()
                .map(|child| (*child, stack, group)),
        );
    }

    debug_assert_eq!(index, fragments.len());
    for context in &mut contexts {
        context.negative.sort_by_key(|(level, _)| *level);
        context.positive.sort_by_key(|(level, _)| *level);
    }
    Context::emit(&contexts, 0, &mut emit);
    top_layer.sort_by_key(|(order, _)| *order);
    for (_, index) in top_layer {
        Context::emit(&contexts, index, &mut emit);
    }
}
