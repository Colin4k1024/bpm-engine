//! Benchmarks for BPMN XML parsing and compilation (#26).
//!
//! Measures:
//! - XML parse speed for various BPMN complexity levels
//! - Compilation speed from AST to ProcessDefinition
//! - End-to-end parse_and_compile speed

use criterion::{black_box, criterion_group, criterion_main, Criterion};

// ---------------------------------------------------------------------------
// BPMN XML fixtures of varying complexity
// ---------------------------------------------------------------------------

const MINIMAL_XML: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="minimal" name="Minimal">
    <startEvent id="start" name="Start"/>
    <endEvent id="end" name="End"/>
    <sequenceFlow id="flow1" sourceRef="start" targetRef="end"/>
  </process>
</definitions>"#;

const SERVICE_TASK_XML: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="payment-flow" name="Payment">
    <startEvent id="start"/>
    <serviceTask id="validate" name="Validate"/>
    <serviceTask id="charge" name="Charge"/>
    <serviceTask id="notify" name="Notify"/>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="validate"/>
    <sequenceFlow id="f2" sourceRef="validate" targetRef="charge"/>
    <sequenceFlow id="f3" sourceRef="charge" targetRef="notify"/>
    <sequenceFlow id="f4" sourceRef="notify" targetRef="end"/>
  </process>
</definitions>"#;

const GATEWAY_XML: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="gateway-flow" name="Gateway">
    <startEvent id="start"/>
    <exclusiveGateway id="decide"/>
    <serviceTask id="path-a" name="Path A"/>
    <serviceTask id="path-b" name="Path B"/>
    <parallelGateway id="fork"/>
    <serviceTask id="parallel-1" name="P1"/>
    <serviceTask id="parallel-2" name="P2"/>
    <parallelGateway id="join"/>
    <endEvent id="end1"/>
    <endEvent id="end2"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="decide"/>
    <sequenceFlow id="f2" sourceRef="decide" targetRef="path-a"/>
    <sequenceFlow id="f3" sourceRef="decide" targetRef="path-b"/>
    <sequenceFlow id="f4" sourceRef="path-a" targetRef="fork"/>
    <sequenceFlow id="f5" sourceRef="fork" targetRef="parallel-1"/>
    <sequenceFlow id="f6" sourceRef="fork" targetRef="parallel-2"/>
    <sequenceFlow id="f7" sourceRef="parallel-1" targetRef="join"/>
    <sequenceFlow id="f8" sourceRef="parallel-2" targetRef="join"/>
    <sequenceFlow id="f9" sourceRef="join" targetRef="end1"/>
    <sequenceFlow id="f10" sourceRef="path-b" targetRef="end2"/>
  </process>
</definitions>"#;

const TIMER_XML: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="timer-flow" name="Timer">
    <startEvent id="start"/>
    <userTask id="task1" name="Approval"/>
    <intermediateCatchEvent id="wait1" name="Wait 1 Hour">
      <timerEventDefinition>
        <timeDuration>PT1H</timeDuration>
      </timerEventDefinition>
    </intermediateCatchEvent>
    <boundaryEvent id="timeout" attachedToRef="task1" cancelActivity="true">
      <timerEventDefinition>
        <timeDuration>PT30S</timeDuration>
      </timerEventDefinition>
    </boundaryEvent>
    <endEvent id="end1"/>
    <endEvent id="end2"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="task1"/>
    <sequenceFlow id="f2" sourceRef="task1" targetRef="wait1"/>
    <sequenceFlow id="f3" sourceRef="wait1" targetRef="end1"/>
    <sequenceFlow id="f4" sourceRef="timeout" targetRef="end2"/>
  </process>
</definitions>"#;

const SUBPROCESS_XML: &str = r#"<definitions xmlns="http://www.omg.org/spec/BPMN/20100524/MODEL">
  <process id="sub-flow" name="SubProcess">
    <startEvent id="start"/>
    <subProcess id="sub1" name="Payment Sub">
      <startEvent id="sub_start"/>
      <serviceTask id="sub_validate" name="Validate"/>
      <serviceTask id="sub_charge" name="Charge"/>
      <endEvent id="sub_end"/>
      <sequenceFlow id="sf1" sourceRef="sub_start" targetRef="sub_validate"/>
      <sequenceFlow id="sf2" sourceRef="sub_validate" targetRef="sub_charge"/>
      <sequenceFlow id="sf3" sourceRef="sub_charge" targetRef="sub_end"/>
    </subProcess>
    <endEvent id="end"/>
    <sequenceFlow id="f1" sourceRef="start" targetRef="sub1"/>
    <sequenceFlow id="f2" sourceRef="sub1" targetRef="end"/>
  </process>
</definitions>"#;

// ---------------------------------------------------------------------------
// Benchmarks: parse only
// ---------------------------------------------------------------------------

fn bench_parse(c: &mut Criterion) {
    c.bench_function("bpmn_parse/minimal", |b| {
        b.iter(|| bpm_engine_bpmn::parse(black_box(MINIMAL_XML)))
    });

    c.bench_function("bpmn_parse/service_tasks", |b| {
        b.iter(|| bpm_engine_bpmn::parse(black_box(SERVICE_TASK_XML)))
    });

    c.bench_function("bpmn_parse/gateways", |b| {
        b.iter(|| bpm_engine_bpmn::parse(black_box(GATEWAY_XML)))
    });

    c.bench_function("bpmn_parse/timer_events", |b| {
        b.iter(|| bpm_engine_bpmn::parse(black_box(TIMER_XML)))
    });

    c.bench_function("bpmn_parse/subprocess", |b| {
        b.iter(|| bpm_engine_bpmn::parse(black_box(SUBPROCESS_XML)))
    });
}

// ---------------------------------------------------------------------------
// Benchmarks: compile only (from pre-parsed model)
// ---------------------------------------------------------------------------

fn bench_compile(c: &mut Criterion) {
    let model_minimal = bpm_engine_bpmn::parse(MINIMAL_XML).unwrap();
    let model_service = bpm_engine_bpmn::parse(SERVICE_TASK_XML).unwrap();
    let model_gateway = bpm_engine_bpmn::parse(GATEWAY_XML).unwrap();
    let model_timer = bpm_engine_bpmn::parse(TIMER_XML).unwrap();
    let model_subprocess = bpm_engine_bpmn::parse(SUBPROCESS_XML).unwrap();

    c.bench_function("bpmn_compile/minimal", |b| {
        b.iter(|| bpm_engine_bpmn::compile(black_box(model_minimal.clone())))
    });

    c.bench_function("bpmn_compile/service_tasks", |b| {
        b.iter(|| bpm_engine_bpmn::compile(black_box(model_service.clone())))
    });

    c.bench_function("bpmn_compile/gateways", |b| {
        b.iter(|| bpm_engine_bpmn::compile(black_box(model_gateway.clone())))
    });

    c.bench_function("bpmn_compile/timer_events", |b| {
        b.iter(|| bpm_engine_bpmn::compile(black_box(model_timer.clone())))
    });

    c.bench_function("bpmn_compile/subprocess", |b| {
        b.iter(|| bpm_engine_bpmn::compile(black_box(model_subprocess.clone())))
    });
}

// ---------------------------------------------------------------------------
// Benchmarks: end-to-end parse_and_compile
// ---------------------------------------------------------------------------

fn bench_parse_and_compile(c: &mut Criterion) {
    c.bench_function("bpmn_e2e/minimal", |b| {
        b.iter(|| bpm_engine_bpmn::parse_and_compile(black_box(MINIMAL_XML)))
    });

    c.bench_function("bpmn_e2e/service_tasks", |b| {
        b.iter(|| bpm_engine_bpmn::parse_and_compile(black_box(SERVICE_TASK_XML)))
    });

    c.bench_function("bpmn_e2e/gateways", |b| {
        b.iter(|| bpm_engine_bpmn::parse_and_compile(black_box(GATEWAY_XML)))
    });

    c.bench_function("bpmn_e2e/timer_events", |b| {
        b.iter(|| bpm_engine_bpmn::parse_and_compile(black_box(TIMER_XML)))
    });

    c.bench_function("bpmn_e2e/subprocess", |b| {
        b.iter(|| bpm_engine_bpmn::parse_and_compile(black_box(SUBPROCESS_XML)))
    });
}

criterion_group!(benches, bench_parse, bench_compile, bench_parse_and_compile);
criterion_main!(benches);
