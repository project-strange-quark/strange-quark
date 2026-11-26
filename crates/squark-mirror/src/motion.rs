// SPDX-License-Identifier: MIT

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
};

use squark_types::{
    NodeId,
    engine::{AnimationEvent, AnimationEventType::*},
};
use style::{
    animation::{
        Animation, AnimationSetKey, AnimationState, DocumentAnimationSet, ElementAnimationSet,
        KeyframesIterationState, Transition,
    },
    dom::OpaqueNode,
    values::computed::{AnimationDirection, AnimationFillMode},
};

use crate::Document;
use style_traits::ToCss;

#[derive(Default)]
pub(crate) struct Motion {
    pub sets: DocumentAnimationSet,
    pub owners: RefCell<BTreeMap<usize, NodeId>>,
    events: Vec<(f64, AnimationEvent)>,
    pub now: f64,
}

impl std::fmt::Debug for Motion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Motion")
            .field("now", &self.now)
            .field("events", &self.events)
            .finish_non_exhaustive()
    }
}

impl Document {
    pub fn set_animation_time(&mut self, seconds: f64) -> bool {
        if seconds <= self.motion.now {
            return false;
        }
        self.motion.now = seconds;
        self.has_running_animations()
    }

    pub fn has_running_animations(&self) -> bool {
        self.motion
            .sets
            .sets
            .read()
            .values()
            .any(ElementAnimationSet::needs_animation_ticks)
    }

    pub fn take_animation_events(&mut self) -> Vec<AnimationEvent> {
        let mut events = std::mem::take(&mut self.motion.events);
        let mut order = BTreeMap::new();
        for (_, event) in &events {
            order.entry(event.target).or_insert_with(|| {
                let mut path = Vec::new();
                let mut id = event.target;
                while let Some(parent) = self.parent(id) {
                    let index = self
                        .children(parent)
                        .unwrap_or_default()
                        .iter()
                        .position(|&child| child == id)
                        .unwrap_or(usize::MAX);
                    path.push(index);
                    id = parent;
                }
                path.reverse();
                path
            });
        }
        events.sort_by(|a, b| {
            let transition = |event: &AnimationEvent| {
                matches!(
                    event.kind,
                    TransitionRun | TransitionStart | TransitionEnd | TransitionCancel
                )
            };
            a.0.total_cmp(&b.0)
                .then_with(|| order[&a.1.target].cmp(&order[&b.1.target]))
                .then_with(|| a.1.pseudo.cmp(&b.1.pseudo))
                .then_with(|| transition(&b.1).cmp(&transition(&a.1)))
                .then_with(|| {
                    if transition(&a.1) && transition(&b.1) {
                        a.1.name.cmp(&b.1.name)
                    } else {
                        std::cmp::Ordering::Equal
                    }
                })
        });
        events.into_iter().map(|(_, event)| event).collect()
    }

    pub fn with_animation_set<R>(
        &self,
        id: NodeId,
        pseudo: Option<style::selector_parser::PseudoElement>,
        read: impl FnOnce(&ElementAnimationSet) -> R,
    ) -> Option<R> {
        let node = self.node(id)?;
        let address = std::ptr::from_ref(node).addr();
        if !self.motion.owners.borrow().contains_key(&address) {
            return None;
        }
        let key = AnimationSetKey::new(OpaqueNode(address), pseudo);
        self.motion.sets.sets.read().get(&key).map(read)
    }

    /// Advance lifetimes before sampling so Stylo sees the right iteration.
    /// Retire completed effects after recascade so transitions can still use
    /// their finish value when comparing the before- and after-change styles.
    pub(crate) fn advance_animations(&mut self, finish: bool) -> BTreeSet<NodeId> {
        let now = self.motion.now;
        let owners = self.motion.owners.borrow();
        let mut targets = BTreeSet::new();
        let sets = self.motion.sets.clone();
        let mut sets = sets.sets.write();
        let mut events = Vec::new();
        sets.retain(|key, set| {
            let Some(&id) = owners.get(&key.node.0) else {
                return false;
            };
            let attached = std::iter::successors(Some(id), |node| self.parent(*node))
                .any(|node| node == self.root);
            if !attached
                || finish
                    && (self
                        .computed_style(id)
                        .is_none_or(|css| css.get_display().is_none())
                        || std::iter::successors(self.parent(id), |node| self.parent(*node)).any(
                            |node| {
                                self.computed_style(node)
                                    .is_some_and(|css| css.get_display().is_none())
                            },
                        )
                        || key
                            .pseudo_element
                            .is_some_and(|pseudo| self.pseudo_style(id, pseudo).is_none()))
            {
                set.cancel_all_animations();
            }
            if set.needs_animation_ticks() {
                targets.insert(id);
            }
            let pseudo = key
                .pseudo_element
                .as_ref()
                .map(cssparser::ToCss::to_css_string);
            let mut emit = |at, kind, name, elapsed_time| {
                events.push((
                    at,
                    AnimationEvent {
                        target: id,
                        pseudo: pseudo.clone(),
                        kind,
                        name,
                        elapsed_time,
                    },
                ));
            };
            for transition in &mut set.transitions {
                transition_events(transition, now, finish, &mut emit);
            }
            for animation in &mut set.animations {
                animation_events(animation, now, finish, &mut emit);
            }
            set.clear_canceled_animations();
            if finish {
                set.transitions
                    .retain(|effect| effect.state != AnimationState::Finished);
            }
            !set.is_empty()
        });
        let live: BTreeSet<_> = sets.keys().map(|key| key.node.0).collect();
        drop(owners);
        self.motion
            .owners
            .get_mut()
            .retain(|address, _| live.contains(address));
        self.motion.events.extend(events);
        targets
    }
}

type Emit<'a> = dyn FnMut(f64, squark_types::engine::AnimationEventType, String, f64) + 'a;

fn transition_events(effect: &mut Transition, now: f64, finish: bool, emit: &mut Emit<'_>) {
    if effect.state == AnimationState::Finished {
        return;
    }
    let name = effect.property_animation.property_id().to_css_string();
    let duration = effect.property_animation.duration;
    let initial = (-effect.delay).clamp(0.0, duration);
    if effect.state == AnimationState::Canceled {
        if !effect.is_new {
            emit(
                now,
                TransitionCancel,
                name,
                (now - effect.start_time).clamp(0.0, duration),
            );
        }
        return;
    }
    if effect.is_new {
        effect.is_new = false;
        emit(
            effect.start_time - effect.delay,
            TransitionRun,
            name.clone(),
            initial,
        );
    }
    if effect.state == AnimationState::Pending && now >= effect.start_time {
        effect.state = AnimationState::Running;
        emit(
            effect.start_time.max(effect.start_time - effect.delay),
            TransitionStart,
            name.clone(),
            initial,
        );
    }
    if finish && effect.state == AnimationState::Running && effect.has_ended(now) {
        effect.state = AnimationState::Finished;
        emit(
            (effect.start_time + duration).max(effect.start_time - effect.delay),
            TransitionEnd,
            name,
            duration,
        );
    }
}

pub(crate) fn iteration(effect: &Animation) -> (f64, f64) {
    match effect.iteration_state {
        KeyframesIterationState::Finite(current, count) => (current, count),
        KeyframesIterationState::Infinite(current) => (current, f64::INFINITY),
    }
}

fn animation_events(effect: &mut Animation, now: f64, finish: bool, emit: &mut Emit<'_>) {
    if effect.state == AnimationState::Finished {
        return;
    }
    let (current, count) = iteration(effect);
    let origin = effect.started_at - current * effect.duration;
    let active_time = (now - origin).max(0.0).min(count * effect.duration);
    if effect.state == AnimationState::Canceled {
        emit(now, AnimationCancel, effect.name.to_string(), active_time);
        return;
    }
    if effect.state == AnimationState::Pending && now >= effect.started_at {
        effect.state = AnimationState::Running;
    }
    if effect.is_new
        && (effect.state == AnimationState::Running
            || matches!(effect.state, AnimationState::Paused(progress) if progress >= 0.0))
    {
        effect.is_new = false;
        emit(
            origin.max(origin - effect.delay),
            AnimationStart,
            effect.name.to_string(),
            (-effect.delay).max(0.0).min(count * effect.duration),
        );
    }
    if effect.state != AnimationState::Running {
        return;
    }
    let next = ((now - origin) / effect.duration)
        .floor()
        .min((count.ceil() - 1.0).max(0.0));
    if next > current {
        effect.started_at = origin + next * effect.duration;
        effect.iteration_state = if count.is_infinite() {
            KeyframesIterationState::Infinite(next)
        } else {
            KeyframesIterationState::Finite(next, count)
        };
        let reverse = match effect.direction {
            AnimationDirection::Normal => false,
            AnimationDirection::Reverse => true,
            AnimationDirection::Alternate => next % 2.0 == 1.0,
            AnimationDirection::AlternateReverse => next % 2.0 == 0.0,
        };
        effect.current_direction = if reverse {
            AnimationDirection::Reverse
        } else {
            AnimationDirection::Normal
        };
        if now < origin + count * effect.duration {
            let elapsed = (current + 1.0) * effect.duration;
            emit(
                origin + elapsed,
                AnimationIteration,
                effect.name.to_string(),
                elapsed,
            );
        }
    }
    if finish && effect.has_ended(now) {
        let duration = count * effect.duration;
        let forwards = matches!(
            effect.fill_mode,
            AnimationFillMode::Forwards | AnimationFillMode::Both
        );
        effect.state = if forwards {
            AnimationState::Paused(effect.current_iteration_end_progress())
        } else {
            AnimationState::Finished
        };
        emit(
            (origin + duration).max(origin - effect.delay),
            AnimationEnd,
            effect.name.to_string(),
            duration,
        );
    }
}
