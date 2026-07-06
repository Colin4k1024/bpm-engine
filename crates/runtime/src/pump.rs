//! Event pump: run_async(initial_event, ctx) -> loop dispatch to handlers (design: handler.md §9).

use std::collections::VecDeque;

use super::handler::{EngineContext, EventHandler};
use bpm_engine_core::EngineEvent;
use tracing::{debug, info_span};

/// Event pump: drives events through handlers until the queue is empty.
pub struct EventPump;

impl EventPump {
    /// Run until queue is empty: pop event, dispatch to all handlers, push new events.
    pub async fn run_async(
        handlers: &[Box<dyn EventHandler>],
        initial: EngineEvent,
        ctx: &mut EngineContext,
    ) {
        let instance_id = match &initial {
            EngineEvent::ProcessStarted(p) => Some(p.instance_id.clone()),
            EngineEvent::TokenArrived(p) => Some(p.instance_id.clone()),
            EngineEvent::TokenCompleted(p) => Some(p.instance_id.clone()),
            EngineEvent::UserTaskCompleted(p) => Some(p.instance_id.clone()),
            EngineEvent::TokenFailed(p) => Some(p.instance_id.clone()),
            EngineEvent::TimerScheduled(p) => Some(p.instance_id.clone()),
            EngineEvent::CallActivityStarted(p) => Some(p.parent_instance_id.clone()),
            EngineEvent::CallActivityCompleted(p) => Some(p.parent_instance_id.clone()),
            EngineEvent::MessageSent(p) => Some(p.instance_id.clone()),
            EngineEvent::SignalSent(p) => Some(p.instance_id.clone()),
            EngineEvent::ProcessTerminated(p) => Some(p.instance_id.clone()),
            EngineEvent::ExternalTaskCompleted(p) => Some(p.instance_id.clone()),
            _ => None,
        };
        let event_name = event_type_name(&initial);

        let _span = info_span!(
            "engine.run_async",
            event = %event_name,
            instance_id = instance_id.as_deref().unwrap_or(""),
        );

        let mut queue: VecDeque<EngineEvent> = VecDeque::new();
        queue.push_back(initial);

        while let Some(event) = queue.pop_front() {
            debug!(event = ?event, "event pump dispatch");
            for handler in handlers {
                let new_events = handler.handle(&event, ctx).await;
                queue.extend(new_events);
            }
        }

        // Record metrics after pump completes
        #[cfg(feature = "observability")]
        {
            metrics::counter!("bpm_events_processed_total").increment(1);
            if event_name == "TimerFired" {
                metrics::counter!("bpm_timer_fired_total").increment(1);
            }
            if event_name == "TokenFailed" {
                metrics::counter!("bpm_engine_errors_total").increment(1);
            }
        }
    }
}

fn event_type_name(ev: &EngineEvent) -> &'static str {
    match ev {
        EngineEvent::ProcessStarted(_) => "ProcessStarted",
        EngineEvent::TokenArrived(_) => "TokenArrived",
        EngineEvent::TokenCompleted(_) => "TokenCompleted",
        EngineEvent::UserTaskCreated(_) => "UserTaskCreated",
        EngineEvent::UserTaskCompleted(_) => "UserTaskCompleted",
        EngineEvent::TimerFired(_) => "TimerFired",
        EngineEvent::TimerScheduled(_) => "TimerScheduled",
        EngineEvent::TokenFailed(_) => "TokenFailed",
        EngineEvent::SagaStarted(_) => "SagaStarted",
        EngineEvent::SagaCompleted(_) => "SagaCompleted",
        EngineEvent::ProcessCompleted(_) => "ProcessCompleted",
        EngineEvent::CallActivityStarted(_) => "CallActivityStarted",
        EngineEvent::CallActivityCompleted(_) => "CallActivityCompleted",
        EngineEvent::MessageSent(_) => "MessageSent",
        EngineEvent::SignalSent(_) => "SignalSent",
        EngineEvent::ProcessTerminated(_) => "ProcessTerminated",
        EngineEvent::ExternalTaskCompleted(_) => "ExternalTaskCompleted",
    }
}
