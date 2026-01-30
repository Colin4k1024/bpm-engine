use crate::model::ProcessInstance;

pub fn validate(instance: &mut ProcessInstance) {
    println!("🔧 validating...");
    instance
        .variables
        .insert("valid".into(), "true".into());
}

#[allow(dead_code)]
pub fn execute(_instance: &mut ProcessInstance) {
    println!("🚀 executing business logic");
}
