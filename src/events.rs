use crate::db::InstanceRepo;
use crate::legacy_engine::Engine;
use crate::model::ProcessDefinition;
use std::error::Error;

/// Events that drive the engine. Process flow is driven by submitting these events
/// (e.g. from main, API, or a queue in the future).
#[derive(Debug)]
pub enum EngineEvent {
    StartProcess {
        process_def_id: String,
        #[allow(dead_code)]
        instance_id: Option<String>,
    },
    CompleteUserTask {
        instance_id: String,
        node_id: String,
    },
}

/// Handle an event: load or create instance, run engine until wait/end, persist.
/// `get_def` returns the process definition for a given id (e.g. from an in-memory registry).
pub fn handle_event<'a, F>(
    event: EngineEvent,
    repo: &InstanceRepo,
    get_def: F,
) -> Result<String, Box<dyn Error>>
where
    F: FnOnce(&str) -> Option<&'a ProcessDefinition>,
{
    match event {
        EngineEvent::StartProcess {
            process_def_id,
            instance_id: _,
        } => {
            let def = get_def(&process_def_id).ok_or("unknown process definition")?;
            let mut instance = Engine::start(def);
            repo.insert_instance(&instance, &instance.process_def_id)?;
            Engine::run_until_wait(def, &mut instance);
            repo.update_instance(&instance)?;
            Ok(instance.id)
        }
        EngineEvent::CompleteUserTask {
            instance_id,
            node_id,
        } => {
            let mut inst = repo
                .get_instance(&instance_id)?
                .ok_or("instance not found")?;
            let def_id = inst.process_def_id.clone();
            let def = get_def(&def_id).ok_or("unknown process definition")?;
            Engine::complete_user_task(def, &mut inst, &node_id);
            repo.update_instance(&inst)?;
            Ok(instance_id)
        }
    }
}
