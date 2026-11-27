// SPDX-License-Identifier: MIT

//! Document-thread task turns for V8. The clock and frame deadlines are host inputs.

use std::collections::VecDeque;
use std::fmt;

use v8::{Function, Global, Local, Object, PinScope, Value};

#[derive(Debug)]
pub enum TaskError {
    Setup(&'static str),
    JavaScript(String),
}

impl fmt::Display for TaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup(message) => formatter.write_str(message),
            Self::JavaScript(message) => write!(formatter, "JavaScript task failed: {message}"),
        }
    }
}

impl std::error::Error for TaskError {}

#[derive(Debug)]
struct Posted {
    ready_at: f64,
    order: u64,
    callback: Global<Function>,
    after_microtasks: Option<Global<Function>>,
}

struct Queue {
    clock: Box<dyn Fn() -> f64>,
    next_order: u64,
    timer: Option<(f64, u64)>,
    posted: VecDeque<Posted>,
}

impl Queue {
    fn order(&mut self) -> u64 {
        let order = self.next_order;
        self.next_order += 1;
        order
    }
}

enum Task {
    Timer,
    Posted(Posted),
}

impl Queue {
    fn next(&mut self, now: f64) -> Option<Task> {
        if let Some(timer) = self.timer
            && timer.0 <= now
            && self
                .posted
                .front()
                .is_none_or(|posted| timer <= (posted.ready_at, posted.order))
        {
            self.timer = None;
            return Some(Task::Timer);
        }
        self.posted.pop_front().map(Task::Posted)
    }
}

/// Returns the private host object used by squark-dom's timing module. Its
/// clock uses monotonic milliseconds from the realm origin.
pub fn install_task_host<'s>(
    scope: &mut PinScope<'s, '_>,
    clock: impl Fn() -> f64 + 'static,
) -> Result<Local<'s, Object>, TaskError> {
    if scope.get_slot::<Queue>().is_some() {
        return Err(TaskError::Setup("timing host already installed"));
    }
    scope.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
    let host = Object::new(scope);
    let bindings = [
        ("now", Function::new(scope, now)),
        ("armTimer", Function::new(scope, arm_timer)),
        ("queueMicrotask", Function::new(scope, queue_microtask)),
        ("postTask", Function::new(scope, post_task)),
        (
            "cloneMessage",
            Function::new(scope, crate::clone_value::clone_message),
        ),
    ];
    for (name, callback) in bindings {
        let key = v8::String::new(scope, name).ok_or(TaskError::Setup("timing binding name"))?;
        let callback = callback.ok_or(TaskError::Setup("timing binding callback"))?;
        if host.set(scope, key.into(), callback.into()) != Some(true) {
            return Err(TaskError::Setup("could not install timing binding"));
        }
    }
    scope.set_slot(Queue {
        clock: Box::new(clock),
        next_order: 0,
        timer: None,
        posted: VecDeque::new(),
    });
    Ok(host)
}

fn now(
    scope: &mut PinScope<'_, '_>,
    _: v8::FunctionCallbackArguments,
    mut result: v8::ReturnValue,
) {
    let value = (scope.get_slot::<Queue>().unwrap().clock)();
    result.set_double(value);
}

fn arm_timer(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    _: v8::ReturnValue,
) {
    let value = args.get(0);
    let deadline = if value.is_null() {
        None
    } else if let Ok(deadline) = Local::<v8::Number>::try_from(value)
        && deadline.value().is_finite()
        && deadline.value() >= 0.0
    {
        Some(deadline.value())
    } else {
        throw_type_error(
            scope,
            "Timer deadline must be a finite nonnegative number or null",
        );
        return;
    };
    let queue = scope.get_slot_mut::<Queue>().unwrap();
    if queue.timer.map(|timer| timer.0) != deadline {
        queue.timer = deadline.map(|deadline| (deadline, queue.order()));
    }
}

fn queue_microtask(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    _: v8::ReturnValue,
) {
    if let Ok(callback) = Local::<Function>::try_from(args.get(0)) {
        scope.enqueue_microtask(callback);
    } else {
        throw_type_error(scope, "queueMicrotask requires a function");
    }
}

fn post_task(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    _: v8::ReturnValue,
) {
    let Ok(callback) = Local::<Function>::try_from(args.get(0)) else {
        throw_type_error(scope, "postTask requires a function");
        return;
    };
    let callback = Global::new(scope, callback);
    let after_microtasks = Local::<Function>::try_from(args.get(1))
        .ok()
        .map(|function| Global::new(scope, function));
    let queue = scope.get_slot_mut::<Queue>().unwrap();
    let ready_at = (queue.clock)();
    let order = queue.order();
    queue.posted.push_back(Posted {
        ready_at,
        order,
        callback,
        after_microtasks,
    });
}

fn throw_type_error(scope: &mut PinScope<'_, '_>, message: &str) {
    if let Some(message) = v8::String::new(scope, message) {
        let error = v8::Exception::type_error(scope, message);
        scope.throw_exception(error);
    }
}

/// The longest idle period, as the requestIdleCallback standard bounds it.
pub const MAX_IDLE_PERIOD_MS: f64 = 50.0;

/// The JS functions that select a timer, the next callback in a frame's rAF
/// snapshot, or the next callback of an idle period.
#[derive(Debug)]
pub struct TaskLoop {
    fire_timer: Global<Function>,
    begin_frame: Global<Function>,
    has_animation: Global<Function>,
    next_animation: Global<Function>,
    idle_wakeup: Global<Function>,
    begin_idle: Global<Function>,
    next_idle: Global<Function>,
}

impl TaskLoop {
    /// Uses the callbacks returned by squark-dom's installTiming in the document realm.
    pub fn new(scope: &mut PinScope<'_, '_>, timing: Local<'_, Object>) -> Result<Self, TaskError> {
        if scope.get_slot::<Queue>().is_none() {
            return Err(TaskError::Setup("install the timing host first"));
        }
        Ok(Self {
            fire_timer: binding(scope, timing, "fireTimer")?,
            begin_frame: binding(scope, timing, "beginFrame")?,
            has_animation: binding(scope, timing, "hasAnimationCallbacks")?,
            next_animation: binding(scope, timing, "nextAnimation")?,
            idle_wakeup: binding(scope, timing, "idleWakeup")?,
            begin_idle: binding(scope, timing, "beginIdle")?,
            next_idle: binding(scope, timing, "nextIdle")?,
        })
    }

    pub fn checkpoint_after_startup(&self, scope: &mut PinScope<'_, '_>) {
        scope.perform_microtask_checkpoint();
    }

    /// The next wakeup for hosts with no immediate posted work.
    pub fn next_timer_deadline(&self, scope: &mut PinScope<'_, '_>) -> Option<f64> {
        scope
            .get_slot::<Queue>()
            .unwrap()
            .timer
            .map(|timer| timer.0)
    }

    pub fn has_posted_tasks(&self, scope: &mut PinScope<'_, '_>) -> bool {
        !scope.get_slot::<Queue>().unwrap().posted.is_empty()
    }

    /// When the next idle period may start, while idle callbacks wait for one.
    pub fn idle_wakeup(&self, scope: &mut PinScope<'_, '_>) -> Option<f64> {
        let wakeup = Local::new(scope, &self.idle_wakeup);
        let value = call(scope, wakeup, &[]).ok()?;
        value
            .is_number()
            .then(|| value.number_value(scope))
            .flatten()
    }

    /// Runs an idle period if one is due and no task is ready. The period ends
    /// at the next timer, after `budget_ms`, or after MAX_IDLE_PERIOD_MS.
    /// Each callback and its microtask checkpoint complete before the deadline
    /// is checked again, as for timers.
    pub fn run_idle(&self, scope: &mut PinScope<'_, '_>, budget_ms: f64) -> Result<(), TaskError> {
        let Some(start) = self.idle_wakeup(scope) else {
            return Ok(());
        };
        let queue = scope.get_slot::<Queue>().unwrap();
        let now = (queue.clock)();
        let timer = queue.timer.map(|timer| timer.0);
        if start > now || !queue.posted.is_empty() || timer.is_some_and(|deadline| deadline <= now)
        {
            return Ok(());
        }
        let period = now + budget_ms.min(MAX_IDLE_PERIOD_MS);
        let deadline = timer.map_or(period, |timer| timer.min(period));
        let begin = Local::new(scope, &self.begin_idle);
        let deadline = v8::Number::new(scope, deadline);
        call(scope, begin, &[deadline.into()])?;
        let next = Local::new(scope, &self.next_idle);
        let mut error = None;
        while let Ok(callback) = Local::<Function>::try_from(call(scope, next, &[])?) {
            let result = call(scope, callback, &[]);
            scope.perform_microtask_checkpoint();
            if let Err(failed) = result {
                error.get_or_insert(failed);
            }
        }
        error.map_or(Ok(()), Err)
    }

    pub(crate) fn has_animation_callbacks(&self, scope: &mut PinScope<'_, '_>) -> bool {
        let has_animation = Local::new(scope, &self.has_animation);
        call(scope, has_animation, &[]).is_ok_and(|value| value.is_true())
    }

    /// Runs a bounded turn, including when no frame tick is pending.
    /// A callback and its checkpoint complete before checking the budget again.
    pub fn run_ready(
        &self,
        scope: &mut PinScope<'_, '_>,
        deadline_ms: f64,
        max_tasks: usize,
    ) -> Result<usize, TaskError> {
        let mut error = None;
        let count = self.run_ready_with(scope, deadline_ms, max_tasks, &mut collect(&mut error));
        error.map_or(Ok(count), Err)
    }

    /// Like `run_ready`, but `invoke` calls each callback and reports its
    /// exception, returning whether it completed. A worker reports uncaught
    /// exceptions as ErrorEvents with their script location.
    pub(crate) fn run_ready_with(
        &self,
        scope: &mut PinScope<'_, '_>,
        deadline_ms: f64,
        max_tasks: usize,
        invoke: &mut Invoke<'_>,
    ) -> usize {
        let mut count = 0;
        while count < max_tasks {
            let queue = scope.get_slot_mut::<Queue>().unwrap();
            let now = (queue.clock)();
            if now >= deadline_ms {
                break;
            }
            let task = queue.next(now);
            let (callback, after_microtasks) = match task {
                Some(Task::Timer) => (Local::new(scope, &self.fire_timer), None),
                Some(Task::Posted(posted)) => {
                    (Local::new(scope, posted.callback), posted.after_microtasks)
                }
                None => break,
            };
            let completed = invoke(scope, callback, &[]);
            scope.perform_microtask_checkpoint();
            if completed && let Some(after) = after_microtasks {
                let after = Local::new(scope, after);
                invoke(scope, after, &[]);
            }
            count += 1;
        }
        count
    }

    /// Runs the frame's task turn and then the rAF snapshot at the tick timestamp.
    pub fn run_frame(
        &self,
        scope: &mut PinScope<'_, '_>,
        vsync_ms: f64,
        deadline_ms: f64,
        max_tasks: usize,
    ) -> Result<(), TaskError> {
        let task_result = self.run_ready(scope, deadline_ms, max_tasks);
        let animation_result = self.run_animation(scope, vsync_ms);
        task_result.and(animation_result)
    }

    pub(crate) fn run_animation(
        &self,
        scope: &mut PinScope<'_, '_>,
        vsync_ms: f64,
    ) -> Result<(), TaskError> {
        let mut error = None;
        self.run_animation_with(scope, vsync_ms, &mut collect(&mut error))?;
        error.map_or(Ok(()), Err)
    }

    /// Runs the rAF snapshot, calling each callback through `invoke`.
    pub(crate) fn run_animation_with(
        &self,
        scope: &mut PinScope<'_, '_>,
        vsync_ms: f64,
        invoke: &mut Invoke<'_>,
    ) -> Result<(), TaskError> {
        let begin = Local::new(scope, &self.begin_frame);
        call(scope, begin, &[])?;
        let timestamp = v8::Number::new(scope, vsync_ms);
        let next = Local::new(scope, &self.next_animation);
        while let Ok(callback) = Local::<Function>::try_from(call(scope, next, &[])?) {
            invoke(scope, callback, &[timestamp.into()]);
            scope.perform_microtask_checkpoint();
        }
        Ok(())
    }
}

/// Calls a task callback with its arguments and returns whether it completed.
pub(crate) type Invoke<'a> =
    dyn for<'s> FnMut(&mut PinScope<'s, '_>, Local<'s, Function>, &[Local<'s, Value>]) -> bool + 'a;

/// Calls through `call`, keeping the first failure.
fn collect(
    error: &mut Option<TaskError>,
) -> impl for<'s> FnMut(&mut PinScope<'s, '_>, Local<'s, Function>, &[Local<'s, Value>]) -> bool + '_
{
    move |scope, callback, args| match call(scope, callback, args) {
        Ok(_) => true,
        Err(failed) => {
            error.get_or_insert(failed);
            false
        }
    }
}

fn binding(
    scope: &mut PinScope<'_, '_>,
    object: Local<'_, Object>,
    name: &'static str,
) -> Result<Global<Function>, TaskError> {
    let key = v8::String::new(scope, name).ok_or(TaskError::Setup(name))?;
    let value = object
        .get(scope, key.into())
        .ok_or(TaskError::Setup(name))?;
    let function = Local::<Function>::try_from(value).map_err(|_| TaskError::Setup(name))?;
    Ok(Global::new(scope, function))
}

pub(crate) fn call<'s>(
    scope: &mut PinScope<'s, '_>,
    function: Local<'s, Function>,
    args: &[Local<'s, Value>],
) -> Result<Local<'s, Value>, TaskError> {
    let receiver = scope.get_current_context().global(scope);
    v8::tc_scope!(let scope, scope);
    function.call(scope, receiver.into(), args).ok_or_else(|| {
        TaskError::JavaScript(scope.exception().map_or_else(
            || "execution stopped".into(),
            |exception| exception.to_rust_string_lossy(scope),
        ))
    })
}
