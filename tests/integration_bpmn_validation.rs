//! BPMN validation: invalid process definitions are rejected by the compiler.

use bpm_engine::bpm_engine_bpmn::{parse_and_compile, CompileError};

#[test]
fn invalid_bpmn_no_start_event_rejected() {
    let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="p">
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="end"/>
  </process>
</definitions>"#;
    let result = parse_and_compile(xml);
    assert!(result.is_err());
    let err = result.unwrap_err();
    if let CompileError::Compile(ce) = &err {
        assert!(!ce.0.is_empty());
        assert!(ce
            .0
            .iter()
            .any(|e| e.code == bpm_engine::bpm_engine_bpmn::ErrorCode::NoStartEvent));
    }
}

#[test]
fn invalid_bpmn_multiple_start_events_rejected() {
    let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="p">
    <startEvent id="s1"/>
    <startEvent id="s2"/>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="s1" targetRef="end"/>
  </process>
</definitions>"#;
    let result = parse_and_compile(xml);
    assert!(result.is_err());
    let err = result.unwrap_err();
    if let CompileError::Compile(ce) = &err {
        assert!(ce
            .0
            .iter()
            .any(|e| e.code == bpm_engine::bpm_engine_bpmn::ErrorCode::MultipleStartEvents));
    }
}

#[test]
fn invalid_bpmn_no_end_event_rejected() {
    let xml = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="p">
    <startEvent id="start"/>
    <task id="t1"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="t1"/>
  </process>
</definitions>"#;
    let result = parse_and_compile(xml);
    assert!(result.is_err());
    let err = result.unwrap_err();
    if let CompileError::Compile(ce) = &err {
        assert!(ce
            .0
            .iter()
            .any(|e| e.code == bpm_engine::bpm_engine_bpmn::ErrorCode::NoEndEvent));
    }
}
