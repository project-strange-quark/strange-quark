// SPDX-License-Identifier: MIT

use std::{
    hash::{DefaultHasher, Hash, Hasher},
    sync::Arc,
    time::{Duration, Instant},
};

use squark_mirror::Document;
use squark_style::Cascade;
use squark_types::{
    NodeId, Size,
    fragment::{
        AnimationDirection, AnimationFill, AnimationKeyframe, CompositorAnimation, Easing,
        FragmentKind, FragmentTree,
    },
};
use style::{
    animation::{
        Animation, AnimationState, ElementAnimationSet, KeyframesIterationState, Transition,
    },
    properties::{ComputedValues, LonghandId, PropertyDeclarationId},
    selector_parser::PseudoElement,
    shared_lock::SharedRwLockReadGuard,
    stylesheets::keyframes_rule::{KeyframesAnimation, KeyframesStepValue},
    values::{
        computed::{
            AnimationComposition, AnimationDirection as CssDirection, AnimationFillMode,
            TimingFunction,
        },
        generics::easing::TimingKeyword,
    },
};
use style_traits::ToCss;

use crate::paint::animated_value;

const PROPERTIES: [LonghandId; 3] = [
    LonghandId::Transform,
    LonghandId::Opacity,
    LonghandId::Filter,
];

pub(crate) fn publish(
    tree: &mut FragmentTree,
    document: &Document,
    cascade: &Cascade<'_>,
    origin: Instant,
) {
    if !document.has_running_animations() {
        return;
    }
    for fragment in tree.nodes.values_mut() {
        let pseudo = match fragment.kind {
            FragmentKind::Box | FragmentKind::Replaced => None,
            FragmentKind::Before => Some(PseudoElement::Before),
            FragmentKind::After => Some(PseudoElement::After),
            _ => continue,
        };
        let owner = fragment.owner;
        let descriptors = document
            .with_animation_set(owner, pseudo, |set| {
                let css = match pseudo {
                    Some(pseudo) => document.pseudo_style(owner, pseudo),
                    None => document.computed_style(owner),
                }?;
                let guard = document.style_lock().read();
                let project = Projection {
                    owner,
                    pseudo,
                    css: &css,
                    size: fragment.border_box.size,
                    origin,
                };
                Some(
                    PROPERTIES
                        .into_iter()
                        .filter_map(|property| {
                            if let Some(transition) =
                                set.transitions.iter().rev().find(|transition| {
                                    transition.property_animation.property_id()
                                        == PropertyDeclarationId::Longhand(property)
                                        && live(&transition.state)
                                })
                            {
                                return project.transition(transition, property);
                            }
                            let mut candidates = set.animations.iter().filter_map(|animation| {
                                let rule = cascade.keyframes(document, owner, &animation.name)?;
                                rule.properties_changed
                                    .iter()
                                    .any(|id| id == PropertyDeclarationId::Longhand(property))
                                    .then_some((animation, rule))
                            });
                            let (animation, rule) = candidates.next()?;
                            (live(&animation.state) && candidates.next().is_none())
                                .then(|| project.animation(animation, &rule, property, &guard))
                                .flatten()
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .flatten();
        if let Some(descriptors) = descriptors.filter(|d| !d.is_empty()) {
            Arc::make_mut(&mut fragment.style).properties.animations =
                descriptors.into_boxed_slice();
        }
    }
}

struct Projection<'a> {
    owner: NodeId,
    pseudo: Option<PseudoElement>,
    css: &'a ComputedValues,
    size: Size,
    origin: Instant,
}

impl Projection<'_> {
    fn transition(
        &self,
        transition: &Transition,
        property: LonghandId,
    ) -> Option<CompositorAnimation> {
        let ui = self.css.get_ui();
        let index = ui
            .transition_property_iter()
            .enumerate()
            .filter(|(_, entry)| entry.is_all() || entry.to_css_string() == property.name())
            .map(|(index, _)| index)
            .last()?;
        let easing = easing(&ui.transition_timing_function_mod(index))?;
        let duration = transition.property_animation.duration;
        if duration <= 0.0 {
            return None;
        }
        let start = transition.start_time;
        let from = transition.calculate_value(start);
        let to = transition.calculate_value(start + duration);
        Some(CompositorAnimation {
            id: self.id(property, "", start),
            start: instant(self.origin, start - transition.delay)?,
            delay_seconds: transition.delay,
            duration: Duration::try_from_secs_f64(duration).ok()?,
            iterations: 1.0,
            direction: AnimationDirection::Normal,
            fill: AnimationFill::Both,
            keyframes: Box::new([
                AnimationKeyframe {
                    offset: 0.0,
                    value: animated_value(&from, self.css, self.size)?,
                    easing,
                },
                AnimationKeyframe {
                    offset: 1.0,
                    value: animated_value(&to, self.css, self.size)?,
                    easing: Easing::Linear,
                },
            ]),
        })
    }

    fn animation(
        &self,
        effect: &Animation,
        rule: &KeyframesAnimation,
        property: LonghandId,
        guard: &SharedRwLockReadGuard<'_>,
    ) -> Option<CompositorAnimation> {
        let ui = self.css.get_ui();
        let index = ui
            .animation_name_iter()
            .position(|name| name.as_atom() == Some(&effect.name))?;
        if ui.animation_composition_mod(index) != AnimationComposition::Replace {
            return None;
        }
        let default_easing = ui.animation_timing_function_mod(index);
        let (iteration, iterations) = match effect.iteration_state {
            KeyframesIterationState::Infinite(current) => (current, f64::INFINITY),
            KeyframesIterationState::Finite(current, count) => (current, count),
        };
        let active_start = effect.started_at - iteration * effect.duration;
        let mut sampler = ElementAnimationSet::default();
        let mut unit = effect.clone();
        unit.started_at = 0.0;
        unit.duration = 1.0;
        unit.iteration_state = KeyframesIterationState::Finite(0.0, 1.0);
        unit.current_direction = CssDirection::Normal;
        unit.fill_mode = AnimationFillMode::Both;
        sampler.animations.push(unit);
        let mut keyframes: Vec<AnimationKeyframe> = Vec::new();
        let property = PropertyDeclarationId::Longhand(property);
        for step in &rule.steps {
            let offset = step.start_offset.percentage.0;
            if offset != 0.0
                && offset != 1.0
                && let KeyframesStepValue::Declarations { block } = &step.value
                && block.read_with(guard).get(property).is_none()
            {
                continue;
            }
            let timing = step.get_animation_timing_function(guard).map_or_else(
                || default_easing.clone(),
                |timing| timing.to_computed_value_without_context(),
            );
            let easing = easing(&timing)?;
            sampler.animations[0].state = AnimationState::Paused(f64::from(offset));
            let values = sampler.get_value_map_for_active_animations(0.0)?;
            let value = values.values().find(|value| value.id() == property)?;
            let frame = AnimationKeyframe {
                offset,
                value: animated_value(value, self.css, self.size)?,
                easing,
            };
            if let Some(last) = keyframes.last_mut().filter(|f| f.offset == offset) {
                *last = frame;
            } else {
                keyframes.push(frame);
            }
        }
        if keyframes.first()?.offset != 0.0 || keyframes.last()?.offset != 1.0 {
            return None;
        }
        Some(CompositorAnimation {
            id: self.id(property.as_longhand()?, effect.name.as_str(), active_start),
            start: instant(self.origin, active_start - effect.delay)?,
            delay_seconds: effect.delay,
            duration: Duration::try_from_secs_f64(effect.duration).ok()?,
            iterations,
            direction: match effect.direction {
                CssDirection::Normal => AnimationDirection::Normal,
                CssDirection::Reverse => AnimationDirection::Reverse,
                CssDirection::Alternate => AnimationDirection::Alternate,
                CssDirection::AlternateReverse => AnimationDirection::AlternateReverse,
            },
            fill: match effect.fill_mode {
                AnimationFillMode::None => AnimationFill::None,
                AnimationFillMode::Forwards => AnimationFill::Forwards,
                AnimationFillMode::Backwards => AnimationFill::Backwards,
                AnimationFillMode::Both => AnimationFill::Both,
            },
            keyframes: keyframes.into_boxed_slice(),
        })
    }

    fn id(&self, property: LonghandId, name: &str, active_start: f64) -> u64 {
        let mut hash = DefaultHasher::new();
        (
            self.owner,
            self.pseudo,
            property,
            name,
            instant(self.origin, active_start),
        )
            .hash(&mut hash);
        hash.finish()
    }
}

fn live(state: &AnimationState) -> bool {
    matches!(state, AnimationState::Pending | AnimationState::Running)
}

fn instant(origin: Instant, seconds: f64) -> Option<Instant> {
    let duration = Duration::try_from_secs_f64(seconds.abs()).ok()?;
    if seconds >= 0.0 {
        origin.checked_add(duration)
    } else {
        origin.checked_sub(duration)
    }
}

fn easing(timing: &TimingFunction) -> Option<Easing> {
    let bezier = match *timing {
        TimingFunction::Keyword(keyword) => match keyword {
            TimingKeyword::Linear => return Some(Easing::Linear),
            TimingKeyword::Ease => [0.25, 0.1, 0.25, 1.0],
            TimingKeyword::EaseIn => [0.42, 0.0, 1.0, 1.0],
            TimingKeyword::EaseOut => [0.0, 0.0, 0.58, 1.0],
            TimingKeyword::EaseInOut => [0.42, 0.0, 0.58, 1.0],
        },
        TimingFunction::CubicBezier { x1, y1, x2, y2 } => [x1, y1, x2, y2],
        // The sampled endpoints of a step function may already have jumped.
        // Keep those tracks on the document thread where Stylo knows the
        // original keyframe values and boundary flags.
        TimingFunction::Steps(..) | TimingFunction::LinearFunction(_) => return None,
    };
    Some(Easing::CubicBezier(bezier))
}
