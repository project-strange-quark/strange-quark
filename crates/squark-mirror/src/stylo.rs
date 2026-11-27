// SPDX-License-Identifier: MIT
//! Adapted from Blitz blitz-dom's stylo/dom.rs and stylo/traversal.rs. See NOTICE.

use std::cell::Cell;
use std::hash::{Hash, Hasher};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};

use markup5ever::{LocalName, Namespace, ns};
use selectors::attr::{AttrSelectorOperation, CaseSensitivity, NamespaceConstraint};
use selectors::matching::{ElementSelectorFlags, MatchingContext, VisitedHandlingMode};
use selectors::sink::Push;
use selectors::{Element as SelectorElement, OpaqueElement};
use squark_types::NodeId;
use style::CaseSensitivityExt as _;
use style::attr::{AttrIdentifier, AttrValue as SnapshotAttr};
use style::context::{
    QuirksMode, RegisteredSpeculativePainter, RegisteredSpeculativePainters, SharedStyleContext,
    StyleContext,
};
use style::data::{ElementData, ElementDataMut, ElementDataRef};
use style::dom::{LayoutIterator, NodeInfo, OpaqueNode, TDocument, TElement, TNode, TShadowRoot};
use style::global_style_data::GLOBAL_STYLE_DATA;
use style::invalidation::element::restyle_hints::RestyleHint;
use style::properties::{
    ComputedValues, Importance, PropertyDeclarationBlock, PropertyId, SourcePropertyDeclaration,
    parse_one_declaration_into, parse_style_attribute,
};
use style::rule_tree::{CascadeLevel, CascadeOrigin, StyleSourceBorrow};
use style::selector_parser::{AttrValue, Lang, NonTSPseudoClass, PseudoElement, SelectorImpl};
use style::selector_parser::{ServoElementSnapshot, SnapshotMap};
use style::servo_arc::{Arc, ArcBorrow};
use style::shared_lock::{Locked, SharedRwLock, StylesheetGuards};
use style::stylesheets::{CssRuleType, Origin, UrlExtraData, layer_rule::LayerOrder};
use style::stylist::{CascadeData, Stylist};
use style::thread_state::{self, ThreadState};
use style::traversal::{DomTraversal, recalc_style_at};
use style::values::{AtomIdent, AtomString, GenericAtomIdent};
use style::{animation::AnimationSetKey, applicable_declarations::ApplicableDeclarationBlock};
use stylo_dom::ElementState;

use super::{Document, Element, ElementSnapshot, Node, NodeKind, StructuralEdit};

/// Stylo's sharing cache requires pointer-sized element handles.
#[derive(Clone, Copy, Debug)]
struct NodeRef<'a>(&'a Node);

thread_local! {
    static TRAVERSAL_DOCUMENT: Cell<Option<NonNull<Document>>> = const { Cell::new(None) };
}

/// Pins the document borrow to this thread for pointer-sized Stylo handles.
struct TraversalTree<'a>(&'a Document);

impl<'a> TraversalTree<'a> {
    fn new(document: &'a Document) -> Self {
        TRAVERSAL_DOCUMENT.with(|owner| {
            assert!(owner.get().is_none(), "nested Stylo document traversal");
            owner.set(Some(NonNull::from(document)));
        });
        Self(document)
    }

    fn document(&self) -> NodeRef<'_> {
        NodeRef(self.0.node(self.0.root).expect("document node"))
    }
}

impl Drop for TraversalTree<'_> {
    fn drop(&mut self) {
        TRAVERSAL_DOCUMENT.with(|owner| owner.set(None));
    }
}

impl<'a> NodeRef<'a> {
    fn with(self, id: NodeId) -> Self {
        Self(self.doc().node(id).expect("live tree link"))
    }

    fn doc(self) -> &'a Document {
        let owner = TRAVERSAL_DOCUMENT
            .with(Cell::get)
            .expect("active style traversal");
        // SAFETY: private NodeRef handles only exist under TraversalTree's borrow.
        // Traversal is serial and the guard rejects overlapping document borrows.
        unsafe { owner.as_ref() }
    }

    fn element(self) -> &'a Element {
        let NodeKind::Element(element) = &self.0.kind else {
            unreachable!("Stylo element handle");
        };
        element
    }
}

impl NodeRef<'_> {
    /// The style traversal walks the flat tree, so its ancestors are the
    /// flat tree's.
    fn mark_ancestors_dirty(self) {
        for parent in
            std::iter::successors(TNode::traversal_parent(&self), TElement::traversal_parent)
        {
            parent.0.dirty_descendants.set(true);
        }
    }

    fn is_shadow_root(self) -> bool {
        matches!(self.0.kind, NodeKind::ShadowRoot { .. })
    }

    fn has_attr(self, name: &str) -> bool {
        self.element()
            .attr(&ns!(), &LocalName::from(name))
            .is_some()
    }

    /// The `exportparts` mappings, inner part name to outer part name.
    fn exported_parts(self) -> Vec<(style::Atom, style::Atom)> {
        let Some(attr) = self.element().attr(&ns!(), &LocalName::from("exportparts")) else {
            return Vec::new();
        };
        attr.scalar
            .split(',')
            .filter_map(|mapping| {
                let mut names = mapping.split(':').map(str::trim);
                let inner = names.next().filter(|name| !name.is_empty())?;
                let outer = names.next().unwrap_or(inner);
                (names.next().is_none()
                    && !outer.is_empty()
                    && !outer.contains(char::is_whitespace))
                .then(|| (style::Atom::from(inner), style::Atom::from(outer)))
            })
            .collect()
    }
}

/// Element children in the flat tree: a host's shadow tree and a slot's
/// assigned nodes, or else the DOM children.
enum FlatChildren<'a> {
    Dom(style::dom::DomChildren<NodeRef<'a>>),
    Listed(std::vec::IntoIter<NodeRef<'a>>),
}

impl<'a> Iterator for FlatChildren<'a> {
    type Item = NodeRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Dom(children) => children.next(),
            Self::Listed(children) => children.next(),
        }
    }
}

fn part_names(element: &Element) -> impl Iterator<Item = style::Atom> {
    element
        .attr(&ns!(), &LocalName::from("part"))
        .into_iter()
        .flat_map(|attr| attr.scalar.split([' ', '\t', '\n', '\r', '\x0c']))
        .filter(|part| !part.is_empty())
        .map(style::Atom::from)
}

fn classes(element: &Element) -> impl Iterator<Item = style::Atom> {
    element
        .attr(&ns!(), &markup5ever::local_name!("class"))
        .into_iter()
        .flat_map(|attr| attr.scalar.split([' ', '\t', '\n', '\r', '\x0c']))
        .filter(|part| !part.is_empty())
        .map(style::Atom::from)
}

fn element_state(element: &Element, inherited_disabled: bool) -> ElementState {
    let mut state = ElementState::empty();
    let bits = element.form_state();
    for (bit, flag) in [
        ElementState::HOVER,
        ElementState::ACTIVE,
        ElementState::FOCUS,
        ElementState::FOCUSRING,
        ElementState::FOCUS_WITHIN,
        ElementState::CHECKED,
        ElementState::INDETERMINATE,
        ElementState::DISABLED,
        ElementState::CHECKED,
        ElementState::READONLY,
        ElementState::PLACEHOLDER_SHOWN,
        ElementState::INVALID,
    ]
    .into_iter()
    .enumerate()
    {
        if bits & (1 << bit) != 0 {
            state.insert(flag);
        }
    }
    if element.namespace == ns!(html) {
        let has = |name| {
            element
                .attr(&ns!(), &markup5ever::LocalName::from(name))
                .is_some()
        };
        let name: &str = &element.name;
        if matches!(name, "a" | "area") && has("href") {
            state.insert(ElementState::UNVISITED);
        }
        if matches!(
            name,
            "button" | "fieldset" | "input" | "optgroup" | "option" | "select" | "textarea"
        ) {
            if inherited_disabled {
                state.insert(ElementState::DISABLED);
            }
            state.set(
                ElementState::ENABLED,
                !state.contains(ElementState::DISABLED),
            );
        }
        if name == "textarea" || name == "input" {
            state.set(
                ElementState::READWRITE,
                !state.contains(ElementState::READONLY),
            );
        } else {
            state.insert(ElementState::READONLY);
        }
    }
    state
}

fn snapshot(previous: &ElementSnapshot, new: &Element) -> ServoElementSnapshot {
    let old = &previous.element;
    let mut result = ServoElementSnapshot::new();
    result.state = Some(element_state(old, previous.inherited_disabled));
    if old.attrs != new.attrs {
        result.attrs = Some(
            old.attrs
                .iter()
                .map(|attr| {
                    let value = match &*attr.name {
                        "id" => SnapshotAttr::from_atomic(attr.scalar.clone()),
                        "class" | "part" => {
                            SnapshotAttr::from_serialized_tokenlist(attr.scalar.clone())
                        }
                        _ => SnapshotAttr::String(attr.scalar.clone()),
                    };
                    (
                        AttrIdentifier {
                            local_name: GenericAtomIdent(attr.name.clone()),
                            name: GenericAtomIdent(attr.name.clone()),
                            namespace: GenericAtomIdent(attr.namespace.clone()),
                            prefix: None,
                        },
                        value,
                    )
                })
                .collect(),
        );
        for attr in old.attrs.iter().chain(&new.attrs) {
            if old.attr(&attr.namespace, &attr.name) != new.attr(&attr.namespace, &attr.name)
                && !result
                    .changed_attrs
                    .contains(GenericAtomIdent::cast(&attr.name))
            {
                result
                    .changed_attrs
                    .push(GenericAtomIdent(attr.name.clone()));
                match &*attr.name {
                    "class" if attr.namespace == ns!() => result.class_changed = true,
                    "id" if attr.namespace == ns!() => result.id_changed = true,
                    _ => result.other_attributes_changed = true,
                }
            }
        }
    }
    result
}

fn invalidate_relative_selectors(
    element: NodeRef<'_>,
    old: &Element,
    stylist: &Stylist,
    snapshots: &SnapshotMap,
) {
    use style::invalidation::element::element_wrapper::Snapshots;
    use style::invalidation::element::relative_selector::RelativeSelectorInvalidator;

    let old_id = old
        .attr(&ns!(), &markup5ever::local_name!("id"))
        .map(|attr| style::Atom::from(attr.scalar.as_str()));
    let old_classes: Vec<_> = classes(old).collect();
    let snapshot = snapshots
        .get(&element)
        .expect("relative invalidation has a snapshot");
    let changed_state = snapshot.state.expect("captured element state") ^ element.state();
    let snapshots = Snapshots::new(snapshots);
    RelativeSelectorInvalidator {
        element,
        quirks_mode: stylist.quirks_mode(),
        snapshots: Some(&snapshots),
        invalidated: |element, _| element.mark_ancestors_dirty(),
        sibling_traversal_map: Default::default(),
    }
    .invalidate_relative_selectors_for_this(
        stylist,
        |element, scope, data, quirks, collector| {
            let map = data.relative_selector_invalidation_map();
            if snapshot.id_changed {
                for id in old_id.as_ref().into_iter().chain(element.id()) {
                    if let Some(dependencies) = map.id_to_selector.get(id, quirks) {
                        for dependency in dependencies {
                            collector.add_dependency(dependency, *element, scope);
                        }
                    }
                }
            }
            if snapshot.class_changed {
                for class in old_classes.iter().chain(&element.0.selector_classes) {
                    if let Some(dependencies) = map.class_to_selector.get(class, quirks) {
                        for dependency in dependencies {
                            collector.add_dependency(dependency, *element, scope);
                        }
                    }
                }
            }
            for attr in &snapshot.changed_attrs {
                if let Some(dependencies) = map.other_attribute_affecting_selectors.get(attr) {
                    for dependency in dependencies {
                        collector.add_dependency(dependency, *element, scope);
                    }
                }
            }
            if !changed_state.is_empty() {
                map.state_affecting_selectors.lookup_with_additional(
                    *element,
                    quirks,
                    old_id.as_ref(),
                    &old_classes,
                    changed_state,
                    |dependency| {
                        if dependency.state.intersects(changed_state) {
                            collector.add_dependency(&dependency.dep, *element, scope);
                        }
                        true
                    },
                );
            }
        },
    );
}

fn inline_style(
    element: &Element,
    lock: &SharedRwLock,
    url: &UrlExtraData,
    quirks: QuirksMode,
) -> Option<Arc<Locked<PropertyDeclarationBlock>>> {
    let parse = |input| parse_style_attribute(input, url, None, quirks, CssRuleType::Style);
    let mut block = element
        .attr(&ns!(), &markup5ever::local_name!("style"))
        .map(|attr| parse(&attr.scalar))
        .unwrap_or_default();
    for declaration in &element.inline {
        let Ok(id) = PropertyId::parse_enabled_for_all_content(&declaration.name) else {
            continue;
        };
        let mut declarations = style::properties::SourcePropertyDeclaration::default();
        if !declaration.value.is_empty()
            && style::properties::parse_one_declaration_into(
                &mut declarations,
                id.clone(),
                &String::from_utf16_lossy(&declaration.value),
                style::stylesheets::Origin::Author,
                url,
                None,
                style_traits::ParsingMode::DEFAULT,
                quirks,
                CssRuleType::Style,
            )
            .is_err()
        {
            continue;
        }
        // CSSOM replaces the old declaration, including when the new priority is lower.
        if let Some(index) = block.first_declaration_to_remove(&id) {
            block.remove_property(&id, index);
        }
        if !declaration.value.is_empty() {
            let importance = if declaration.important {
                style::properties::Importance::Important
            } else {
                style::properties::Importance::Normal
            };
            block.extend(declarations.drain(), importance);
        }
    }
    (!block.declarations().is_empty()).then(|| Arc::new(lock.wrap(block)))
}

const SVG_PAINT_ATTRIBUTES: [&str; 3] = ["color", "fill", "stroke"];

fn svg_paint_hints(
    element: &Element,
    lock: &SharedRwLock,
    url: &UrlExtraData,
) -> Option<Arc<Locked<PropertyDeclarationBlock>>> {
    if element.namespace != ns!(svg) {
        return None;
    }
    let mut block = PropertyDeclarationBlock::default();
    for attribute in &element.attrs {
        if attribute.namespace != ns!() || !SVG_PAINT_ATTRIBUTES.contains(&attribute.name.as_ref())
        {
            continue;
        }
        let Ok(id) = PropertyId::parse_enabled_for_all_content(&attribute.name) else {
            continue;
        };
        let mut declarations = SourcePropertyDeclaration::default();
        if parse_one_declaration_into(
            &mut declarations,
            id,
            &attribute.scalar,
            Origin::Author,
            url,
            None,
            style_traits::ParsingMode::DEFAULT,
            QuirksMode::NoQuirks,
            CssRuleType::Style,
        )
        .is_ok()
        {
            block.extend(declarations.drain(), Importance::Normal);
        }
    }
    (!block.is_empty()).then(|| Arc::new(lock.wrap(block)))
}

impl PartialEq for NodeRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}

impl Eq for NodeRef<'_> {}

impl Hash for NodeRef<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::ptr::hash(self.0, state);
    }
}

impl<'a> TDocument for NodeRef<'a> {
    type ConcreteNode = Self;

    fn as_node(&self) -> Self {
        *self
    }
    fn is_html_document(&self) -> bool {
        self.doc().is_html
    }
    fn quirks_mode(&self) -> QuirksMode {
        self.doc().quirks
    }
    fn shared_lock(&self) -> &SharedRwLock {
        self.doc().style_lock()
    }
}

impl NodeInfo for NodeRef<'_> {
    fn is_element(&self) -> bool {
        matches!(self.0.kind, NodeKind::Element(_))
    }
    fn is_text_node(&self) -> bool {
        matches!(self.0.kind, NodeKind::Text(_))
    }
}

impl<'a> TShadowRoot for NodeRef<'a> {
    type ConcreteNode = Self;

    fn as_node(&self) -> Self {
        *self
    }
    fn host(&self) -> Self {
        let host = self
            .doc()
            .shadow_host(self.0.id)
            .expect("a live shadow host");
        self.with(host)
    }
    fn style_data<'b>(&self) -> Option<&'b CascadeData>
    where
        Self: 'b,
    {
        self.doc().shadow_styles(self.0.id).map(|data| &**data)
    }
}

impl<'a> TNode for NodeRef<'a> {
    type ConcreteElement = Self;
    type ConcreteDocument = Self;
    type ConcreteShadowRoot = Self;

    fn parent_node(&self) -> Option<Self> {
        self.0.parent.map(|id| self.with(id))
    }
    fn first_child(&self) -> Option<Self> {
        self.0.children.first().map(|&id| self.with(id))
    }
    fn last_child(&self) -> Option<Self> {
        self.0.children.last().map(|&id| self.with(id))
    }
    fn prev_sibling(&self) -> Option<Self> {
        self.0.prev.map(|id| self.with(id))
    }
    fn next_sibling(&self) -> Option<Self> {
        self.0.next.map(|id| self.with(id))
    }
    fn owner_doc(&self) -> Self {
        self.with(self.doc().root)
    }

    fn is_in_document(&self) -> bool {
        self.doc().is_connected(self.0.id)
    }

    fn traversal_parent(&self) -> Option<Self> {
        self.with(self.doc().flat_parent(self.0.id)?).as_element()
    }
    fn opaque(&self) -> OpaqueNode {
        OpaqueNode(std::ptr::from_ref(self.0).addr())
    }
    fn debug_id(self) -> usize {
        self.0.id.raw() as usize
    }
    fn as_element(&self) -> Option<Self> {
        self.is_element().then_some(*self)
    }
    fn as_document(&self) -> Option<Self> {
        matches!(self.0.kind, NodeKind::Document).then_some(*self)
    }
    fn as_shadow_root(&self) -> Option<Self> {
        self.is_shadow_root().then_some(*self)
    }
}

impl SelectorElement for NodeRef<'_> {
    type Impl = SelectorImpl;

    fn opaque(&self) -> OpaqueElement {
        OpaqueElement::from_ptr(NonNull::from(self.0).cast())
    }
    fn parent_element(&self) -> Option<Self> {
        self.parent_node()?.as_element()
    }
    fn parent_node_is_shadow_root(&self) -> bool {
        self.parent_node().is_some_and(NodeRef::is_shadow_root)
    }
    fn containing_shadow_host(&self) -> Option<Self> {
        Some(TElement::containing_shadow(self)?.host())
    }
    fn assigned_slot(&self) -> Option<Self> {
        Some(self.with(self.doc().assigned_slot(self.0.id)?))
    }
    fn is_pseudo_element(&self) -> bool {
        false
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        std::iter::successors(self.prev_sibling(), TNode::prev_sibling).find(NodeInfo::is_element)
    }

    fn next_sibling_element(&self) -> Option<Self> {
        std::iter::successors(self.next_sibling(), TNode::next_sibling).find(NodeInfo::is_element)
    }

    fn first_element_child(&self) -> Option<Self> {
        self.dom_children().find(NodeInfo::is_element)
    }
    fn is_html_element_in_html_document(&self) -> bool {
        self.doc().is_html && self.is_html_element()
    }
    fn has_local_name(&self, name: &LocalName) -> bool {
        self.local_name() == name
    }
    fn has_namespace(&self, namespace: &Namespace) -> bool {
        self.namespace() == namespace
    }
    fn is_same_type(&self, other: &Self) -> bool {
        self.local_name() == other.local_name() && self.namespace() == other.namespace()
    }

    fn attr_matches(
        &self,
        namespace: &NamespaceConstraint<&GenericAtomIdent<markup5ever::NamespaceStaticSet>>,
        name: &GenericAtomIdent<markup5ever::LocalNameStaticSet>,
        operation: &AttrSelectorOperation<&AtomString>,
    ) -> bool {
        self.element().attrs.iter().any(|attr| {
            let namespace_matches = match namespace {
                NamespaceConstraint::Any => true,
                NamespaceConstraint::Specific(ns) => attr.namespace == ns.0,
            };
            namespace_matches && attr.name == name.0 && operation.eval_str(&attr.scalar)
        })
    }

    fn match_non_ts_pseudo_class(
        &self,
        pseudo: &NonTSPseudoClass,
        _: &mut MatchingContext<Self::Impl>,
    ) -> bool {
        match pseudo {
            NonTSPseudoClass::Lang(lang) => self.match_element_lang(None, lang),
            NonTSPseudoClass::Visited => false,
            _ => self.state().intersects(pseudo.state_flag()),
        }
    }

    fn match_pseudo_element(&self, _: &PseudoElement, _: &mut MatchingContext<Self::Impl>) -> bool {
        false
    }

    fn apply_selector_flags(&self, flags: ElementSelectorFlags) {
        let own = &self.0.selector_flags;
        own.set(own.get() | flags.for_self().bits());
        if let Some(parent) = self.parent_node() {
            let parent = &parent.0.selector_flags;
            parent.set(parent.get() | flags.for_parent().bits());
        }
    }

    fn is_link(&self) -> bool {
        element_state(self.element(), false).contains(ElementState::UNVISITED)
    }
    fn is_html_slot_element(&self) -> bool {
        self.is_html_element() && self.local_name() == &markup5ever::local_name!("slot")
    }

    fn has_id(&self, id: &AtomIdent, case_sensitivity: CaseSensitivity) -> bool {
        self.id()
            .is_some_and(|own| case_sensitivity.eq_atom(own, id))
    }

    fn has_class(&self, class: &AtomIdent, case_sensitivity: CaseSensitivity) -> bool {
        self.0
            .selector_classes
            .iter()
            .any(|own| case_sensitivity.eq_atom(own, class))
    }

    fn imported_part(&self, name: &AtomIdent) -> Option<AtomIdent> {
        self.exported_parts()
            .into_iter()
            .find(|(_, outer)| outer == &name.0)
            .map(|(inner, _)| GenericAtomIdent(inner))
    }
    fn is_part(&self, name: &AtomIdent) -> bool {
        part_names(self.element()).any(|part| part == name.0)
    }
    fn is_empty(&self) -> bool {
        self.dom_children().all(|child| match &child.0.kind {
            NodeKind::Text(text) => text.is_empty(),
            NodeKind::Element(_) => false,
            _ => true,
        })
    }
    fn is_root(&self) -> bool {
        self.parent_node()
            .is_some_and(|parent| parent.0.id == self.doc().root)
    }
    fn has_custom_state(&self, _: &AtomIdent) -> bool {
        false
    }
    fn add_element_unique_hashes(&self, filter: &mut selectors::bloom::BloomFilter) -> bool {
        style::bloom::each_relevant_element_hash(*self, |hash| {
            filter.insert_hash(hash & selectors::bloom::BLOOM_HASH_MASK);
        });
        true
    }
}

impl<'a> TElement for NodeRef<'a> {
    type ConcreteNode = Self;
    type TraversalChildrenIterator = FlatChildren<'a>;

    fn as_node(&self) -> Self {
        *self
    }
    fn traversal_children(&self) -> LayoutIterator<Self::TraversalChildrenIterator> {
        let document = self.doc();
        let id = self.0.id;
        if document.shadow_root(id).is_none() && !document.is_slot(id) {
            return LayoutIterator(FlatChildren::Dom(self.dom_children()));
        }
        let children = document.flat_children(id);
        LayoutIterator(FlatChildren::Listed(
            children
                .iter()
                .map(|&child| self.with(child))
                .collect::<Vec<_>>()
                .into_iter(),
        ))
    }
    fn inheritance_parent(&self) -> Option<Self> {
        TElement::traversal_parent(self)
    }

    fn is_html_element(&self) -> bool {
        self.has_namespace(&ns!(html))
    }
    fn is_mathml_element(&self) -> bool {
        self.has_namespace(&ns!(mathml))
    }
    fn is_svg_element(&self) -> bool {
        self.has_namespace(&ns!(svg))
    }
    fn style_attribute(&self) -> Option<ArcBorrow<'_, Locked<PropertyDeclarationBlock>>> {
        self.0.inline_style.as_ref().map(Arc::borrow_arc)
    }
    fn state(&self) -> ElementState {
        element_state(self.element(), self.doc().inherited_disabled(self.0.id))
    }
    fn has_part_attr(&self) -> bool {
        self.has_attr("part")
    }
    fn exports_any_part(&self) -> bool {
        self.has_attr("exportparts")
    }
    fn id(&self) -> Option<&style::Atom> {
        self.0.selector_id.as_ref()
    }

    fn each_class<F: FnMut(&AtomIdent)>(&self, mut callback: F) {
        for class in &self.0.selector_classes {
            callback(AtomIdent::cast(class));
        }
    }

    fn each_attr_name<F: FnMut(&style::LocalName)>(&self, mut callback: F) {
        for attr in &self.element().attrs {
            callback(GenericAtomIdent::cast(&attr.name));
        }
    }

    fn each_custom_state<F: FnMut(&AtomIdent)>(&self, _: F) {}

    fn each_part<F: FnMut(&AtomIdent)>(&self, mut callback: F) {
        for part in part_names(self.element()) {
            callback(AtomIdent::cast(&part));
        }
    }

    fn each_exported_part<F: FnMut(&AtomIdent)>(&self, name: &AtomIdent, mut callback: F) {
        for (inner, outer) in self.exported_parts() {
            if inner == name.0 {
                callback(AtomIdent::cast(&outer));
            }
        }
    }
    fn has_dirty_descendants(&self) -> bool {
        self.0.dirty_descendants.get()
    }
    fn has_snapshot(&self) -> bool {
        self.doc().snapshots.contains_key(&self.0.id)
    }
    fn handled_snapshot(&self) -> bool {
        self.0.handled_snapshot.get()
    }

    // SAFETY: The serial traversal has exclusive access to the snapshot flag.
    unsafe fn set_handled_snapshot(&self) {
        self.0.handled_snapshot.set(true);
    }

    // SAFETY: Stylo supplies exclusive traversal access to the element. The flag is a Cell.
    unsafe fn set_dirty_descendants(&self) {
        self.0.dirty_descendants.set(true);
    }

    // SAFETY: Stylo supplies exclusive traversal access to the element. The flag is a Cell.
    unsafe fn unset_dirty_descendants(&self) {
        self.0.dirty_descendants.set(false);
    }

    fn store_children_to_process(&self, _: isize) {
        unreachable!("preorder-only traversal")
    }
    fn did_process_child(&self) -> isize {
        unreachable!("preorder-only traversal")
    }

    // SAFETY: The sole traversal owns the element's style data and has no outstanding data borrow.
    unsafe fn ensure_data(&self) -> ElementDataMut<'_> {
        self.0.has_data.set(true);
        self.0.style.borrow_mut()
    }

    // SAFETY: Stylo clears data only when the element has no outstanding data borrows.
    unsafe fn clear_data(&self) {
        if self.0.has_data.replace(false) {
            *self.0.style.borrow_mut() = ElementData::default();
        }
    }

    fn has_data(&self) -> bool {
        self.0.has_data.get()
    }
    fn borrow_data(&self) -> Option<ElementDataRef<'_>> {
        self.has_data()
            .then(|| self.0.style.borrow())
            .filter(|data| data.has_styles())
    }
    fn mutate_data(&self) -> Option<ElementDataMut<'_>> {
        self.has_data().then(|| self.0.style.borrow_mut())
    }

    fn skip_item_display_fixup(&self) -> bool {
        false
    }
    fn may_have_animations(&self) -> bool {
        self.doc()
            .motion
            .owners
            .borrow()
            .contains_key(&TNode::opaque(self).0)
    }
    fn set_may_have_animations(&self) {
        self.doc()
            .motion
            .owners
            .borrow_mut()
            .insert(TNode::opaque(self).0, self.0.id);
    }
    fn has_animations(&self, context: &SharedStyleContext<'_>) -> bool {
        context.animations.has_active_animations_or_transitions(
            &AnimationSetKey::new_for_non_pseudo(TNode::opaque(self)),
        )
    }
    fn has_css_animations(
        &self,
        context: &SharedStyleContext<'_>,
        pseudo: Option<PseudoElement>,
    ) -> bool {
        context
            .animations
            .has_active_animations(&AnimationSetKey::new(TNode::opaque(self), pseudo))
    }
    fn has_css_transitions(
        &self,
        context: &SharedStyleContext<'_>,
        pseudo: Option<PseudoElement>,
    ) -> bool {
        context
            .animations
            .has_active_transitions(&AnimationSetKey::new(TNode::opaque(self), pseudo))
    }
    fn animation_rule(
        &self,
        context: &SharedStyleContext<'_>,
    ) -> Option<Arc<Locked<PropertyDeclarationBlock>>> {
        context.animations.get_animation_declarations(
            &AnimationSetKey::new_for_non_pseudo(TNode::opaque(self)),
            context.current_time_for_animations,
            &self.doc().style_lock,
        )
    }
    fn transition_rule(
        &self,
        context: &SharedStyleContext<'_>,
    ) -> Option<Arc<Locked<PropertyDeclarationBlock>>> {
        context.animations.get_transition_declarations(
            &AnimationSetKey::new_for_non_pseudo(TNode::opaque(self)),
            context.current_time_for_animations,
            &self.doc().style_lock,
        )
    }
    fn shadow_root(&self) -> Option<Self> {
        Some(self.with(self.doc().shadow_root(self.0.id)?))
    }
    fn containing_shadow(&self) -> Option<Self> {
        Some(self.with(self.doc().containing_shadow_root(self.0.id)?))
    }

    fn get_attr(&self, name: &style::LocalName, namespace: &style::Namespace) -> Option<String> {
        self.element()
            .attr(&namespace.0, &name.0)
            .map(|attr| attr.scalar.clone())
    }

    fn lang_attr(&self) -> Option<AttrValue> {
        let element = self.element();
        element
            .attr(&ns!(xml), &markup5ever::local_name!("lang"))
            .or_else(|| element.attr(&ns!(), &markup5ever::local_name!("lang")))
            .map(|attr| AttrValue::from(attr.scalar.as_str()))
    }
    fn match_element_lang(&self, override_lang: Option<Option<AttrValue>>, lang: &Lang) -> bool {
        let language = override_lang.unwrap_or_else(|| {
            std::iter::successors(Some(*self), TNode::parent_element)
                .find_map(|node| node.lang_attr())
        });
        language.is_some_and(|value| {
            style::selector_parser::extended_filtering(value.as_ref(), lang.as_ref())
        })
    }

    fn is_html_document_body_element(&self) -> bool {
        self.is_html_element()
            && self.local_name() == &markup5ever::local_name!("body")
            && SelectorElement::parent_element(self).is_some_and(|parent| parent.is_root())
    }

    fn synthesize_presentational_hints_for_legacy_attributes<'h, V>(
        &self,
        _: VisitedHandlingMode,
        hints: &mut V,
    ) where
        V: Push<ApplicableDeclarationBlock<'h>>,
        Self: 'h,
    {
        if let Some(block) = self.0.svg_paint_hints.as_ref() {
            hints.push(ApplicableDeclarationBlock::from_declarations(
                StyleSourceBorrow::from_declarations(Arc::borrow_arc(block)),
                CascadeLevel::new(CascadeOrigin::PresHints),
                LayerOrder::root(),
            ));
        }
    }

    fn has_unique_presentational_hints(&self) -> bool {
        self.0.svg_paint_hints.is_some()
    }

    fn local_name(&self) -> &LocalName {
        &self.element().name
    }
    fn namespace(&self) -> &Namespace {
        &self.element().namespace
    }
    fn query_container_size(
        &self,
        _: &style::values::specified::Display,
    ) -> euclid::default::Size2D<Option<app_units::Au>> {
        Default::default()
    }

    fn has_selector_flags(&self, flags: ElementSelectorFlags) -> bool {
        self.0.selector_flags.get() & flags.bits() == flags.bits()
    }

    fn relative_selector_search_direction(&self) -> ElementSelectorFlags {
        let flags = ElementSelectorFlags::from_bits_retain(self.0.selector_flags.get());
        for direction in [
            ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_ANCESTOR_SIBLING,
            ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_ANCESTOR,
            ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_SIBLING,
        ] {
            if flags.contains(direction) {
                return direction;
            }
        }
        ElementSelectorFlags::empty()
    }
}

struct NoPaintWorklets;

impl RegisteredSpeculativePainters for NoPaintWorklets {
    fn get(&self, _: &style::Atom) -> Option<&dyn RegisteredSpeculativePainter> {
        None
    }
}

struct RecalcStyle<'a>(SharedStyleContext<'a>, &'a AtomicUsize);

impl<'doc> DomTraversal<NodeRef<'doc>> for RecalcStyle<'_> {
    fn process_preorder<F: FnMut(NodeRef<'doc>)>(
        &self,
        context: &mut StyleContext<NodeRef<'doc>>,
        node: NodeRef<'doc>,
        note_child: F,
    ) {
        if let Some(element) = node.as_element() {
            // SAFETY: traverse_dom runs without a worker pool and this callback owns the
            // current element's style data for the duration of recalc_style_at.
            let mut data = unsafe { element.ensure_data() };
            self.1.fetch_add(1, Ordering::Relaxed);
            recalc_style_at(self, context, element, &mut data, note_child);
            element.0.dirty_descendants.set(false);
        }
    }

    fn needs_postorder_traversal() -> bool {
        false
    }
    fn process_postorder(&self, _: &mut StyleContext<NodeRef<'doc>>, _: NodeRef<'doc>) {
        unreachable!("preorder-only traversal");
    }
    fn shared_context(&self) -> &SharedStyleContext<'_> {
        &self.0
    }
}

impl Document {
    pub fn keyframes(
        &self,
        stylist: &Stylist,
        id: NodeId,
        name: &style::Atom,
    ) -> Option<style::stylesheets::keyframes_rule::KeyframesAnimation> {
        let _tree = TraversalTree::new(self);
        let element = NodeRef(self.node(id)?).as_element()?;
        stylist.lookup_keyframes(name, element).cloned()
    }

    /// Recompute the entire document, for a new device or stylesheet set.
    pub fn cascade(&mut self, stylist: &mut Stylist, url: &UrlExtraData) {
        self.update_styles(stylist, url, true);
    }

    pub fn restyle(&mut self, stylist: &mut Stylist, url: &UrlExtraData) {
        self.update_styles(stylist, url, false);
    }

    fn update_styles(&mut self, stylist: &mut Stylist, url: &UrlExtraData, reset: bool) {
        let measure = std::env::var_os("SQUARK_ENGINE_TIMINGS").is_some_and(|v| v == "1");
        let timer = || measure.then(std::time::Instant::now);
        let start = timer();
        let animation_targets = self.advance_animations(false);
        let lock = self.style_lock.clone();
        let quirks = self.quirks;
        let structural_edits = std::mem::take(&mut self.structural_edits);
        let relocated = std::mem::take(&mut self.relocated);
        let selector_dirty = std::mem::take(&mut self.selector_dirty);
        if reset {
            self.snapshots.clear();
            for node in self.live_nodes_mut() {
                if node.has_data.get() {
                    node.style
                        .borrow_mut()
                        .hint
                        .insert(RestyleHint::restyle_subtree());
                    node.dirty_descendants.set(true);
                }
                node.selector_flags.set(0);
                refresh_selector_data(node, &lock, url, quirks);
            }
        } else {
            for id in selector_dirty {
                if let Some(super::Slot::Live(node)) = self.slots.get_mut(&id.slot())
                    && node.id == id
                {
                    refresh_selector_data(node, &lock, url, quirks);
                }
            }
        }

        let after_prepare = timer();
        let visited = AtomicUsize::new(0);
        let tree = TraversalTree::new(self);
        if let Some(root) = tree.document().first_element_child() {
            let _layout = LayoutThread::enter();
            let guard = lock.read();
            let guards = StylesheetGuards {
                author: &guard,
                ua_or_user: &guard,
            };
            stylist.flush(&guards).process_style(root, None);
            if !reset {
                self.invalidate_structure(structural_edits, relocated);
            }
            for id in animation_targets {
                if let Some(node) = self.node(id).filter(|node| node.has_data.get()) {
                    node.style
                        .borrow_mut()
                        .hint
                        .insert(RestyleHint::RESTYLE_SELF);
                    NodeRef(node).mark_ancestors_dirty();
                }
            }

            let mut snapshot_map = SnapshotMap::new();
            for (&id, old) in &self.snapshots {
                let Some(node) = self.node(id) else {
                    continue;
                };
                let Some(new) = node.element() else {
                    continue;
                };
                let element = NodeRef(node);
                if !element.is_in_document() {
                    continue;
                }
                node.handled_snapshot.set(false);
                snapshot_map.insert(TNode::opaque(&element), snapshot(old, new));
                let old = &old.element;
                if old.inline != new.inline
                    || old.attr(&ns!(), &markup5ever::local_name!("style"))
                        != new.attr(&ns!(), &markup5ever::local_name!("style"))
                    || (new.namespace == ns!(svg)
                        && SVG_PAINT_ATTRIBUTES.iter().any(|name| {
                            let name = LocalName::from(*name);
                            old.attr(&ns!(), &name) != new.attr(&ns!(), &name)
                        }))
                {
                    node.style
                        .borrow_mut()
                        .hint
                        .insert(RestyleHint::RESTYLE_SELF);
                }
                element.mark_ancestors_dirty();
                // Stylo finds the elements a changed slot's ::slotted() rules
                // match through slotted_nodes(), which this mirror leaves
                // empty, so restyle the slot's assigned elements directly.
                if SelectorElement::is_html_slot_element(&element) {
                    for &slotted in self.assigned_nodes(id).iter() {
                        if let Some(slotted) = self.node(slotted).filter(|node| node.has_data.get())
                        {
                            slotted
                                .style
                                .borrow_mut()
                                .hint
                                .insert(RestyleHint::RESTYLE_SELF);
                            NodeRef(slotted).mark_ancestors_dirty();
                        }
                    }
                }
            }
            for (&id, old) in &self.snapshots {
                if let Some(element) = self.node(id).map(NodeRef)
                    && snapshot_map.get(&element).is_some()
                {
                    invalidate_relative_selectors(element, &old.element, stylist, &snapshot_map);
                }
            }
            let after_invalidation = timer();
            let context = SharedStyleContext {
                traversal_flags: style::traversal_flags::TraversalFlags::empty(),
                stylist,
                options: GLOBAL_STYLE_DATA.options.clone(),
                guards,
                visited_styles_enabled: false,
                animations: self.motion.sets.clone(),
                current_time_for_animations: self.motion.now,
                snapshot_map: &snapshot_map,
                registered_speculative_painters: &NoPaintWorklets,
            };
            let token = RecalcStyle::pre_traverse(root, &context);
            if token.should_traverse() {
                style::driver::traverse_dom(&RecalcStyle(context, &visited), token, None);
            }
            if let (Some(start), Some(prepared), Some(invalidated)) =
                (start, after_prepare, after_invalidation)
            {
                eprintln!(
                    "squark-style prepare_us={} invalidate_us={} traverse_us={} elements={}",
                    (prepared - start).as_micros(),
                    (invalidated - prepared).as_micros(),
                    invalidated.elapsed().as_micros(),
                    visited.load(Ordering::Relaxed)
                );
            }
        }
        drop(tree);
        for &id in self.snapshots.keys() {
            if let Some(node) = self.node(id) {
                node.handled_snapshot.set(false);
            }
        }
        self.snapshots.clear();
        self.restyled_elements = visited.load(Ordering::Relaxed);
        self.advance_animations(true);
    }

    /// Changed child lists can alter sibling selectors. Relative selectors can
    /// also affect ancestors or their neighboring branches.
    fn invalidate_structure(
        &self,
        edits: impl IntoIterator<Item = (NodeId, StructuralEdit)>,
        relocated: impl IntoIterator<Item = NodeId>,
    ) {
        let hint = |element: NodeRef<'_>, hint| {
            if element.0.has_data.get() {
                element.0.style.borrow_mut().hint.insert(hint);
            }
            element.mark_ancestors_dirty();
        };
        let subtree = RestyleHint::restyle_subtree();
        for id in relocated {
            if let Some(element) = self
                .node(id)
                .map(NodeRef)
                .and_then(|node| node.as_element())
            {
                if element.is_in_document() {
                    hint(element, subtree);
                } else if element.0.has_data.get() {
                    // A later attachment will recascade descendants with new parents.
                    element.0.style.borrow_mut().hint.insert(subtree);
                }
            }
        }
        let flags = |element: NodeRef<'_>| {
            ElementSelectorFlags::from_bits_retain(element.0.selector_flags.get())
        };
        let mut ancestor_walked = std::collections::HashSet::new();
        let mut preceding_walked = std::collections::HashSet::new();
        let mut anchors_checked = std::collections::HashSet::new();
        let mut following_hinted = std::collections::HashSet::new();
        for (id, edit) in edits {
            let Some(parent) = self.node(id).map(NodeRef) else {
                continue;
            };
            if !parent.is_in_document() {
                continue;
            }
            let parent_flags = flags(parent);
            if edit.membership_changed {
                let children = || parent.dom_children().filter_map(|node| node.as_element());
                if parent_flags.intersects(
                    ElementSelectorFlags::HAS_SLOW_SELECTOR
                        | ElementSelectorFlags::MAY_HAVE_TREE_COUNTING_FUNCTION,
                ) {
                    for child in children() {
                        hint(child, subtree);
                    }
                } else if parent_flags
                    .contains(ElementSelectorFlags::HAS_SLOW_SELECTOR_LATER_SIBLINGS)
                {
                    let next = parent
                        .dom_children()
                        .find(|node| edit.following.contains(&node.0.id));
                    for node in std::iter::successors(next, TNode::next_sibling)
                        .filter_map(|node| node.as_element())
                    {
                        hint(node, subtree);
                    }
                }
                if parent_flags.contains(ElementSelectorFlags::HAS_EDGE_CHILD_SELECTOR) {
                    let first = parent.first_element_child();
                    let last = parent
                        .last_child()
                        .and_then(|node| node.as_element().or_else(|| node.prev_sibling_element()));
                    let previous = edit.old_edges.into_iter().flatten().filter_map(|id| {
                        let node = self.node(id).map(NodeRef)?;
                        (node.parent_node() == Some(parent)).then_some(node)
                    });
                    for edge in previous.chain(first).chain(last) {
                        hint(edge, subtree);
                    }
                }
            }
            let Some(parent) = parent.as_element() else {
                continue;
            };
            let mut mark_following = |element: NodeRef<'_>| {
                for next in std::iter::successors(element.next_sibling_element(), |sibling| {
                    sibling.next_sibling_element()
                }) {
                    if !following_hinted.insert(next.0.id) {
                        break;
                    }
                    hint(next, subtree);
                }
            };
            let empty_changed = edit.was_empty != SelectorElement::is_empty(&parent);
            if empty_changed && flags(parent).contains(ElementSelectorFlags::HAS_EMPTY_SELECTOR) {
                hint(parent, subtree);
                mark_following(parent);
            }
            let mut consider_anchor = |element: NodeRef<'_>| {
                if !anchors_checked.insert(element.0.id) {
                    return;
                }
                let style_flags = flags(element);
                if style_flags.contains(ElementSelectorFlags::ANCHORS_RELATIVE_SELECTOR_NON_SUBJECT)
                {
                    hint(element, subtree);
                    mark_following(element);
                } else if style_flags.contains(ElementSelectorFlags::ANCHORS_RELATIVE_SELECTOR) {
                    hint(element, RestyleHint::RESTYLE_SELF);
                }
            };
            if !edit.membership_changed && !empty_changed {
                continue;
            }
            // The tree is stable throughout invalidation. An earlier walk
            // through this ancestor already covered its preceding siblings and
            // ancestors, even if many children were replaced in one batch.
            for ancestor in std::iter::successors(Some(parent), TNode::parent_element) {
                if !ancestor_walked.insert(ancestor.0.id) {
                    break;
                }
                consider_anchor(ancestor);
                for prior in std::iter::successors(ancestor.prev_sibling_element(), |sibling| {
                    sibling.prev_sibling_element()
                }) {
                    if !preceding_walked.insert(prior.0.id) {
                        break;
                    }
                    consider_anchor(prior);
                }
            }
        }
    }

    /// Populate cached `id` and `class` atoms for selector queries on a
    /// document that has not run a full style pass.
    pub fn refresh_selectors(&mut self) {
        for id in std::mem::take(&mut self.selector_dirty) {
            if let Some(super::Slot::Live(node)) = self.slots.get_mut(&id.slot())
                && node.id == id
                && let Some(element) = node.dom.element()
            {
                node.selector_id = element
                    .attr(&ns!(), &markup5ever::local_name!("id"))
                    .map(|attr| style::Atom::from(attr.scalar.as_str()));
                node.selector_classes = classes(element).collect();
            }
        }
    }

    pub fn select(
        &self,
        scope: NodeId,
        source: &str,
        mode: squark_types::engine::SelectMode,
        url: &UrlExtraData,
    ) -> Result<squark_types::engine::QueryValue, squark_types::engine::EngineError> {
        use selectors::context::{
            MatchingForInvalidation, MatchingMode, NeedsSelectorFlags, SelectorCaches,
        };
        use squark_types::engine::{EngineError, QueryValue, SelectMode};
        use style::selector_parser::SelectorParser;

        let selectors = SelectorParser::parse_author_origin_no_namespace(source, url)
            .map_err(|_| EngineError::InvalidSelector)?;
        let node = self.node(scope).ok_or(EngineError::MissingNode(scope))?;
        let _tree = TraversalTree::new(self);
        let node = NodeRef(node);
        let mut caches = SelectorCaches::default();
        let mut context = MatchingContext::new(
            MatchingMode::Normal,
            None,
            &mut caches,
            self.quirks,
            NeedsSelectorFlags::No,
            MatchingForInvalidation::No,
        );
        context.scope_element = node
            .as_element()
            .map(|element| SelectorElement::opaque(&element));
        let mut matches =
            |element| selectors::matching::matches_selector_list(&selectors, element, &mut context);
        match mode {
            SelectMode::Match => Ok(QueryValue::Bool(node.as_element().is_some_and(matches))),
            SelectMode::Closest => {
                let found = std::iter::successors(node.as_element(), TNode::parent_element)
                    .find(|&element| matches(element))
                    .map(|found| found.0.id);
                Ok(QueryValue::Node(found))
            }
            SelectMode::First => {
                let found = node
                    .dom_descendants()
                    .filter_map(|child| child.as_element())
                    .find(|&element| matches(element))
                    .map(|found| found.0.id);
                Ok(QueryValue::Node(found))
            }
            SelectMode::All => Ok(QueryValue::Nodes(
                node.dom_descendants()
                    .filter_map(|child| child.as_element())
                    .filter(|&element| matches(element))
                    .map(|element| element.0.id)
                    .collect(),
            )),
        }
    }

    pub fn compute_declarations(
        &self,
        stylist: &Stylist,
        environment: Option<NodeId>,
        block: PropertyDeclarationBlock,
    ) -> Arc<ComputedValues> {
        let _layout = LayoutThread::enter();
        let _tree = TraversalTree::new(self);
        let parent = environment.and_then(|id| self.computed_style(id));
        let parent = parent
            .as_deref()
            .unwrap_or_else(|| stylist.device().default_computed_values());
        let guard = self.style_lock.read();
        let guards = StylesheetGuards {
            author: &guard,
            ua_or_user: &guard,
        };
        stylist.compute_for_declarations::<NodeRef<'_>>(
            &guards,
            parent,
            Arc::new(self.style_lock.wrap(block)),
        )
    }

    pub fn query_pseudo_style(
        &self,
        id: NodeId,
        pseudo: &PseudoElement,
        stylist: &Stylist,
    ) -> Option<Arc<ComputedValues>> {
        let originating = self.computed_style(id)?;
        if let Some(css) = self.pseudo_style(id, *pseudo) {
            return Some(css);
        }
        let _layout = LayoutThread::enter();
        let _tree = TraversalTree::new(self);
        let node = NodeRef(self.node(id)?).as_element()?;
        let guard = self.style_lock.read();
        let guards = StylesheetGuards {
            author: &guard,
            ua_or_user: &guard,
        };
        if pseudo.is_lazy()
            && let Some(css) = stylist.lazily_compute_pseudo_element_style(
                &guards,
                node,
                pseudo,
                style::stylist::RuleInclusion::All,
                &originating,
                true,
                None,
            )
        {
            return Some(css);
        }
        Some(stylist.compute_pseudo_element_style_with_inputs(
            style::context::CascadeInputs::default(),
            pseudo,
            &guards,
            Some(&originating),
            Some(node),
        ))
    }

    pub fn computed_style(&self, id: NodeId) -> Option<Arc<ComputedValues>> {
        self.node(id)?.style.borrow().styles.get_primary().cloned()
    }
    pub fn pseudo_style(&self, id: NodeId, pseudo: PseudoElement) -> Option<Arc<ComputedValues>> {
        if !pseudo.is_eager() {
            return None;
        }
        self.node(id)?
            .style
            .borrow()
            .styles
            .pseudos
            .get(&pseudo)
            .cloned()
    }
}

fn refresh_selector_data(
    node: &mut Node,
    lock: &SharedRwLock,
    url: &UrlExtraData,
    quirks: QuirksMode,
) {
    if let Some(element) = node.dom.element() {
        node.selector_id = element
            .attr(&ns!(), &markup5ever::local_name!("id"))
            .map(|attr| style::Atom::from(attr.scalar.as_str()));
        node.selector_classes = classes(element).collect();
        node.inline_style = inline_style(element, lock, url, quirks);
        node.svg_paint_hints = svg_paint_hints(element, lock, url);
    }
}

struct LayoutThread;

impl LayoutThread {
    fn enter() -> Self {
        thread_state::enter(ThreadState::LAYOUT);
        Self
    }
}

impl Drop for LayoutThread {
    fn drop(&mut self) {
        thread_state::exit(ThreadState::LAYOUT);
    }
}
