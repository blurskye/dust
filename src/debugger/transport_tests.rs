//! Transport tests for dap connections and protocol messages.

use crate::debugger::*;
use crate::process::read_content_length_message;
use std::time::Instant;

pub(crate) const ADAPTER: &str = r#"
import json,sys,pathlib,time
seq=0
def send(value):
 global seq
 seq+=1
 value['seq']=seq
 data=json.dumps(value).encode()
 sys.stdout.buffer.write(('Content-Length: %d\r\n\r\n'%len(data)).encode()+data)
 sys.stdout.buffer.flush()
def read():
 headers={}
 while True:
  line=sys.stdin.buffer.readline()
  if not line: return None
  if line in (b'\r\n',b'\n'): break
  k,v=line.decode().split(':',1); headers[k.lower()]=v.strip()
 return json.loads(sys.stdin.buffer.read(int(headers['content-length'])))
while True:
 m=read()
 if m is None: break
 if m.get('type')=='response' and m.get('command')=='runInTerminal':
  pathlib.Path('reverse-reply').write_text(json.dumps(m));continue
 if m.get('type')!='request': continue
 c=m['command']; body={}
 if c=='launch' and 'program' in m.get('arguments',{}): pathlib.Path('launch-program').write_text(m['arguments']['program'])
 if c=='evaluate':
  send({'type':'response','request_seq':m['seq'],'command':c,'success':False,'message':'owned evaluation rejected'});continue
 if c=='threads':
  pathlib.Path('flood-start').write_text('started')
  for n in range(300): send({'type':'event','event':'output','body':{'output':str(n)+'\n'}})
  pathlib.Path('flood-finished').write_text('finished')
  body={'threads':[{'id':1,'name':'original-thread'}]}
 if c=='stackTrace': body={'stackFrames':[{'id':1,'name':'original-frame','line':1,'column':1,'source':{'sourceReference':7}}]}
 if c=='source': body={'name':'owned-source','content':'original adapter source\n'}
 send({'type':'response','request_seq':m['seq'],'command':c,'success':True,'body':body})
 if c in ('launch','attach'): send({'type':'event','event':'stopped','body':{'reason':'fixture','threadId':1}})
 if c=='source': send({'type':'request','command':'runInTerminal','arguments':{'cwd':str(pathlib.Path.cwd()),'args':['/owned-disabled-fixture'],'env':{}}})
"#;
/// Python 3 for test fixtures; Windows installs it as `python`.
pub(crate) fn python() -> PathBuf {
    let name = if cfg!(windows) { "python" } else { "python3" };
    crate::tools::discover_executable(name).expect("test fixtures need Python 3")
}
fn launch(root: &Path, script: &Path) -> DapLaunch {
    DapLaunch {
        adapter: "same-name".into(),
        executable: python(),
        arguments: vec!["-u".into(), script.display().to_string()],
        // Windows cannot open sockets in a process without SYSTEMROOT.
        environment: std::env::vars()
            .filter(|(name, _)| name.eq_ignore_ascii_case("SYSTEMROOT"))
            .collect(),
        path_maps: BTreeMap::new(),
        root: root.into(),
        request: "launch".into(),
        request_arguments: json!({}),
        breakpoints: Vec::new(),
    }
}
fn wait(mut predicate: impl FnMut() -> bool) {
    // Each call takes at most one event, and sleeps are coarse on loaded CI machines.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(Instant::now() < deadline, "owned adapter deadline");
        thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn adapter_identifiers_with_nul_are_rejected_before_spawning() {
    let root = tempfile::tempdir().unwrap();
    let mut configuration = launch(root.path(), &root.path().join("unused.py"));
    configuration.adapter = "bad\0adapter".into();
    let mut service = DapClient::new();
    assert!(matches!(
        service.start_absolute(configuration),
        Err(DapError::InvalidConfiguration)
    ));
    assert!(!service.is_running());
    assert_eq!(service.pending(), 0);
}

#[test]
fn breakpoints_reach_the_adapter_before_the_program_runs() {
    let prefix = ADAPTER.split("while True:\n m=read()").next().unwrap();
    let body = r#"
log=open('requests.log','a',buffering=1)
while True:
 m=read()
 if m is None: break
 if m.get('type')!='request': continue
 c=m['command']
 log.write(c+'\n')
 body={'supportsConfigurationDoneRequest':True} if c=='initialize' else {'breakpoints':[{'verified':True}]} if c=='setBreakpoints' else {}
 if c=='setBreakpoints':
  while not pathlib.Path('breakpoints-go').exists(): time.sleep(.002)
 send({'type':'response','request_seq':m['seq'],'command':c,'success':True,'body':body})
 if c=='launch':
  while not pathlib.Path('configure').exists(): time.sleep(.002)
  send({'type':'event','event':'initialized','body':{}})
"#;
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("startup.py");
    std::fs::write(&script, format!("{prefix}\n{body}")).unwrap();
    let mut configuration = launch(root.path(), &script);
    configuration.breakpoints = (0..96)
        .map(|index| {
            let source = root.path().join(format!("program{index}.py"));
            (
                source.clone(),
                vec![DapSourceBreakpoint {
                    path: source,
                    line: 2,
                    column: None,
                    condition: None,
                    hit_condition: None,
                    log_message: None,
                }],
            )
        })
        .collect();
    let mut service = DapClient::new();
    service.start_absolute(configuration).unwrap();
    wait(|| matches!(service.try_event(), Some(DapEvent::Running { .. })));
    service.writer_gate(true);
    service.control(DapControl::Scopes { frame_id: 1 }).unwrap();
    wait(|| service.writer_waiting());
    // Startup sends one breakpoint at a time, so fill the writer with independent
    // controls before letting initialization proceed.
    for _ in 0..32 {
        service.control(DapControl::Scopes { frame_id: 1 }).unwrap();
    }
    wait(|| service.writes_queued() == 32);
    std::fs::write(root.path().join("configure"), "go").unwrap();
    assert!(service.is_running());
    service.writer_gate(false);
    wait(|| {
        request_log(root.path())
            .iter()
            .any(|line| line == "setBreakpoints")
    });
    assert!(
        !request_log(root.path())
            .iter()
            .any(|line| line == "configurationDone")
    );
    std::fs::write(root.path().join("breakpoints-go"), "go").unwrap();
    let mut answers = 0;
    wait(|| {
        while let Some(event) = service.try_event() {
            match event {
                DapEvent::Response {
                    response: DapResponse::Breakpoints(_),
                    ..
                } => answers += 1,
                DapEvent::Failed { message, .. } => panic!("{message}"),
                _ => {}
            }
        }
        answers == 96
            && request_log(root.path())
                .iter()
                .any(|line| line == "configurationDone")
    });
    let log = request_log(root.path());
    assert_eq!(
        log.iter().filter(|line| *line == "setBreakpoints").count(),
        96
    );
    let position = |name: &str| log.iter().position(|line| line == name).expect(name);
    assert!(
        log.iter()
            .rposition(|line| line == "setBreakpoints")
            .unwrap()
            < position("configurationDone"),
        "breakpoints come first, or the program runs past them: {log:?}"
    );
    service.stop().unwrap();
}
#[test]
fn startup_breakpoint_edits_replace_the_launch_snapshot() {
    let prefix = ADAPTER.split("while True:\n m=read()").next().unwrap();
    let body = r#"
while True:
 m=read()
 if m is None: break
 c=m['command']; body={}
 if c=='initialize': body={'supportsConfigurationDoneRequest':True}
 if c=='threads':
  body={'threads':[]}
  send({'type':'event','event':'initialized','body':{}})
 if c=='setBreakpoints':
  assert m['arguments']['breakpoints']==[], 'removed startup breakpoint came back'
  body={'breakpoints':[]}
 if c=='configurationDone': pathlib.Path('configured').write_text('done')
 send({'type':'response','request_seq':m['seq'],'command':c,'success':True,'body':body})
"#;
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("coalesce.py");
    std::fs::write(&script, format!("{prefix}\n{body}")).unwrap();
    let path = root.path().join("program.py");
    let mut configuration = launch(root.path(), &script);
    configuration.breakpoints = vec![(
        path.clone(),
        vec![DapSourceBreakpoint {
            path: path.clone(),
            line: 2,
            column: None,
            condition: None,
            hit_condition: None,
            log_message: None,
        }],
    )];
    let mut service = DapClient::new();
    service.start_absolute(configuration).unwrap();
    wait(|| matches!(service.try_event(), Some(DapEvent::Running { .. })));
    service
        .control(DapControl::SetBreakpoints {
            path,
            breakpoints: vec![],
        })
        .unwrap();
    service.control(DapControl::Threads).unwrap();
    wait(|| {
        if let Some(DapEvent::Failed { message, .. }) = service.try_event() {
            panic!("{message}");
        }
        root.path().join("configured").exists()
    });
    service.stop().unwrap();
}

#[test]
fn adapter_startup_failures_include_bounded_stderr() {
    let prefix = ADAPTER.split("while True:\n m=read()").next().unwrap();
    for rejected in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("failure.py");
        let rejection = if rejected {
            "m=read()\nsend({'type':'response','request_seq':m['seq'],'command':'initialize','success':False})\ntime.sleep(30)\n"
        } else {
            ""
        };
        std::fs::write(&script, format!("{prefix}\nsys.stderr.write('x'*20000+' missing library libadapter.so\\n')\nsys.stderr.flush()\n{rejection}")).unwrap();
        let mut service = DapClient::new();
        service
            .start_absolute(launch(root.path(), &script))
            .unwrap();
        wait(|| match service.try_event() {
            Some(DapEvent::Failed { message, .. }) => {
                assert!(
                    message.contains("missing library libadapter.so"),
                    "{message}"
                );
                assert!(message.len() < 9000);
                if rejected {
                    assert!(message.contains("initialize rejected"), "{message}");
                }
                true
            }
            _ => false,
        });
        service.stop().unwrap();
        wait(|| service.pending() == 0);
    }
}

#[test]
fn slow_launches_do_not_reuse_the_initialize_deadline() {
    let prefix = ADAPTER.split("while True:\n m=read()").next().unwrap();
    let body = r#"
while True:
 m=read()
 if m is None: break
 c=m['command']
 if c=='launch': time.sleep(16)
 send({'type':'response','request_seq':m['seq'],'command':c,'success':True,'body':{}})
"#;
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("slow-launch.py");
    std::fs::write(&script, format!("{prefix}\n{body}")).unwrap();
    let mut service = DapClient::new();
    service
        .start_absolute(launch(root.path(), &script))
        .unwrap();
    // The launch alone takes 16 seconds; leave room for a loaded machine.
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        match service.try_event() {
            Some(DapEvent::Running { .. }) => break,
            Some(DapEvent::Failed { message, .. }) => panic!("{message}"),
            _ => {}
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    service.stop().unwrap();
}

#[test]
fn debuggee_exit_preserves_final_output_until_termination_or_eof() {
    let prefix = ADAPTER.split("while True:\n m=read()").next().unwrap();
    for explicit_termination in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("exit.py");
        let termination = if explicit_termination {
            "  send({'type':'event','event':'terminated','body':{}})\n"
        } else {
            ""
        };
        std::fs::write(&script, format!("{prefix}\nwhile True:\n m=read()\n if m is None: break\n c=m['command']\n send({{'type':'response','request_seq':m['seq'],'command':c,'success':True,'body':{{}}}})\n if c=='launch':\n  send({{'type':'event','event':'exited','body':{{'exitCode':0}}}})\n  send({{'type':'event','event':'output','body':{{'output':'final output'}}}})\n{termination}  break\n")).unwrap();
        let mut service = DapClient::new();
        service
            .start_absolute(launch(root.path(), &script))
            .unwrap();
        let mut output = String::new();
        wait(|| match service.try_event() {
            Some(DapEvent::Output { output: text, .. }) => {
                output.push_str(&text);
                false
            }
            Some(DapEvent::Terminated { .. }) => {
                assert_eq!(output, "final output");
                true
            }
            Some(DapEvent::Failed { message, .. }) => panic!("{message}"),
            _ => false,
        });
        service.stop().unwrap();
    }
}

#[test]
fn adapter_exit_drains_buffered_launch_rejection_after_output() {
    let prefix = ADAPTER.split("while True:\n m=read()").next().unwrap();
    let body = r#"
import os
while True:
 m=read()
 if m is None: break
 c=m['command']
 if c=='launch':
  for n in range(200): send({'type':'event','event':'output','body':{'output':str(n)}})
  send({'type':'response','request_seq':m['seq'],'command':c,'success':False,'message':'buffered launch rejection'})
  os._exit(1)
 send({'type':'response','request_seq':m['seq'],'command':c,'success':True,'body':{}})
"#;
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("buffered-exit.py");
    std::fs::write(&script, format!("{prefix}\n{body}")).unwrap();
    let mut service = DapClient::new();
    service
        .start_absolute(launch(root.path(), &script))
        .unwrap();
    let mut outputs = 0;
    wait(|| match service.try_event() {
        Some(DapEvent::Output { .. }) => {
            outputs += 1;
            false
        }
        Some(DapEvent::Failed { message, .. }) => {
            assert!(message.contains("buffered launch rejection"), "{message}");
            assert_eq!(outputs, 200);
            true
        }
        _ => false,
    });
    service.stop().unwrap();
}

#[test]
fn a_rejection_says_why_rather_than_only_that_it_failed() {
    let delve = json!({"message":"Failed to launch","body":{"error":{"format":"Failed to launch: {why}","variables":{"why":"Build error"}}}});
    assert_eq!(rejection_detail(&delve), "Failed to launch: Build error");
    assert_eq!(
        rejection_detail(&json!({"message":"no such file"})),
        "no such file"
    );
}
#[test]
fn rejection_variables_are_not_templates_and_output_is_bounded() {
    let response = json!({"body":{"error":{
        "format":"{a} {b} {missing}",
        "variables":{"a":"{b}{b}", "b":"literal"}
    }}});
    assert_eq!(rejection_detail(&response), "{b}{b} literal {missing}");
    let response = json!({"body":{"error":{
        "format":"{a}{a}", "variables":{"a":"é".repeat(5000)}
    }}});
    assert_eq!(
        rejection_detail(&response),
        format!("{}\n[output truncated]\n", "é".repeat(4096))
    );
}

#[test]
fn stopping_lets_the_adapter_end_its_program_and_clean_up() {
    let prefix = ADAPTER.split("while True:\n m=read()").next().unwrap();
    let body = r#"
log=open('requests.log','a',buffering=1)
while True:
 m=read()
 if m is None: break
 if m.get('type')!='request': continue
 log.write(m['command']+'\n')
 send({'type':'response','request_seq':m['seq'],'command':m['command'],'success':True,'body':{}})
 if m['command']=='launch': send({'type':'event','event':'initialized','body':{}})
time.sleep(0.3)
log.write('cleaned up\n')
"#;
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("cleanup.py");
    std::fs::write(&script, format!("{prefix}\n{body}")).unwrap();
    let mut service = DapClient::new();
    service
        .start_absolute(launch(root.path(), &script))
        .unwrap();
    wait(|| request_log(root.path()).iter().any(|line| line == "launch"));
    service.stop().unwrap();
    wait(|| {
        request_log(root.path())
            .iter()
            .any(|line| line == "cleaned up")
    });
}
#[test]
fn startup_reply_survives_stop_and_wrong_commands_cannot_consume_requests() {
    let prefix = ADAPTER.split("while True:\n m=read()").next().unwrap();
    for request in ["launch", "attach"] {
        for success in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let script = root.path().join("ordered.py");
            let body = r#"
while True:
 m=read()
 if m is None: break
 if m.get('type')!='request': continue
 c=m['command']
 if c in ('launch','attach'):
  send({'type':'response','request_seq':m['seq'],'command':'wrong-command','success':False})
  send({'type':'event','event':'stopped','body':{'reason':'entry','threadId':1}})
  send({'type':'event','event':'output','body':{'output':'before-ack'}})
  while not pathlib.Path('ack-go').exists(): time.sleep(.002)
  send({'type':'response','request_seq':m['seq'],'command':c,'success':SUCCESS,'message':'original startup verdict','body':{}})
  send({'type':'response','request_seq':m['seq'],'command':c,'success':False,'message':'duplicate must be ignored'})
  send({'type':'event','event':'output','body':{'output':'after-ack'}})
  continue
 if c=='initialize':
  send({'type':'response','request_seq':m['seq'],'command':'launch','success':False,'message':'foreign initialize command'})
 if c=='scopes':
  send({'type':'response','request_seq':m['seq'],'command':'evaluate','success':False,'message':'foreign command'})
 body={'threads':[]} if c=='threads' else {'scopes':[]} if c=='scopes' else {}
 send({'type':'response','request_seq':m['seq'],'command':c,'success':True,'body':body})
 if c=='scopes': send({'type':'response','request_seq':m['seq'],'command':c,'success':False,'message':'duplicate scopes'})
"#;
            std::fs::write(
                &script,
                format!(
                    "{prefix}\nSUCCESS={}\n{body}",
                    if success { "True" } else { "False" }
                ),
            )
            .unwrap();
            let mut service = DapClient::new();
            let mut configuration = launch(root.path(), &script);
            configuration.request = request.into();
            service.start_absolute(configuration).unwrap();
            let mut stopped = false;
            wait(|| match service.try_event() {
                Some(DapEvent::Stopped { .. }) => {
                    stopped = true;
                    false
                }
                Some(DapEvent::Output { output, .. }) => output == "before-ack",
                Some(DapEvent::Running { .. }) => panic!("running before the launch verdict"),
                Some(DapEvent::Failed { message, .. }) => {
                    panic!("wrong command consumed launch: {message}")
                }
                _ => false,
            });
            assert!(stopped);
            std::fs::write(root.path().join("ack-go"), "go").unwrap();
            wait(|| match service.try_event() {
                Some(DapEvent::Failed { message, .. }) => {
                    assert!(
                        !success,
                        "duplicate or wrong command consumed a request: {message}"
                    );
                    assert!(message.contains("original startup verdict"), "{message}");
                    true
                }
                Some(DapEvent::Running { .. }) => {
                    panic!("launch acknowledgement erased paused state")
                }
                Some(DapEvent::Output { output, .. }) if output == "after-ack" => {
                    assert!(success, "the failed original startup reply was discarded");
                    true
                }
                _ => false,
            });
            if success {
                service.control(DapControl::Scopes { frame_id: 1 }).unwrap();
                wait(|| match service.try_event() {
                    Some(DapEvent::Response {
                        response: DapResponse::Scopes(rows),
                        ..
                    }) => {
                        assert!(rows.is_empty());
                        true
                    }
                    Some(
                        DapEvent::RequestFailed { message, .. } | DapEvent::Failed { message, .. },
                    ) => panic!("{message}"),
                    _ => false,
                });
                // A successful continue response suffices; adapters need not emit continued.
                service.control(DapControl::Continue).unwrap();
                wait(|| match service.try_event() {
                    Some(DapEvent::Running { .. }) => true,
                    Some(
                        DapEvent::RequestFailed { message, .. } | DapEvent::Failed { message, .. },
                    ) => panic!("{message}"),
                    _ => false,
                });
            }
            service.stop().unwrap();
            wait(|| service.pending() == 0);
        }
    }
}

#[test]
fn dap_result_queue_blocks_the_producer_until_results_are_drained() {
    let (mut service, adapter) = session::fixture();
    let (progress, received) = std::sync::mpsc::channel();
    let producer = thread::spawn(move || {
        for n in 0..32 {
            adapter.event(
                0,
                DapEvent::Output {
                    adapter: "same-name".into(),
                    output: format!("{n}\n"),
                },
            );
        }
        progress.send("full").unwrap();
        adapter.event(
            0,
            DapEvent::Output {
                adapter: "same-name".into(),
                output: "32\n".into(),
            },
        );
        progress.send("resumed").unwrap();
    });
    assert_eq!(
        received.recv_timeout(Duration::from_secs(3)).unwrap(),
        "full"
    );
    assert_eq!(
        received.recv_timeout(Duration::from_millis(100)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    );
    assert!(
        matches!(service.try_event(), Some(DapEvent::Output { output, .. }) if output == "0\n")
    );
    assert_eq!(
        received.recv_timeout(Duration::from_secs(3)).unwrap(),
        "resumed"
    );
    producer.join().unwrap();
    for n in 1..=32 {
        assert!(
            matches!(service.try_event(), Some(DapEvent::Output { output, .. }) if output == format!("{n}\n"))
        );
    }
    assert!(service.try_event().is_none());
}

#[test]
fn dap_control_queue_reports_full_while_the_adapter_is_live() {
    // Keep the consumer idle so writer failure cannot race control-queue saturation.
    let (service, adapter) = session::fixture();
    let mut full = false;
    for _ in 0..256 {
        match service.control(DapControl::Threads) {
            Ok(()) => {}
            Err(DapError::Full) => {
                full = true;
                break;
            }
            Err(error) => panic!("adapter failed before its queue filled: {error}"),
        }
    }
    assert!(full, "producer queue must be bounded");
    assert!(matches!(adapter.control(), Some(DapControl::Threads)));
    service.control(DapControl::Threads).unwrap();
}

#[test]
fn dap_owned_transport_backpressures_results_and_stops_nonreading_adapter() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("adapter.py");
    // Exceed both the event/message queues and OS pipe buffers so the adapter itself blocks.
    let adapter = ADAPTER.replace("str(n)+'\\n'", "str(n)+' '+('x'*4000)+'\\n'");
    std::fs::write(&script, adapter).unwrap();
    let mut service = DapClient::new();
    service
        .start_absolute(launch(root.path(), &script))
        .unwrap();
    let old = service.context().unwrap();
    wait(|| root.path().join("flood-start").exists());
    let undrained_until = Instant::now() + Duration::from_millis(250);
    while Instant::now() < undrained_until {
        assert!(
            !root.path().join("flood-finished").exists(),
            "undrained results did not block the producer"
        );
        thread::sleep(Duration::from_millis(2));
    }
    // Actual JSON/process stdout, undrained beyond the bounded event queue.
    let mut outputs = Vec::new();
    let mut response = false;
    wait(|| {
        for _ in 0..32 {
            match service.try_event() {
                Some(DapEvent::Output { output, .. }) => outputs.push(output),
                Some(DapEvent::Response {
                    response: DapResponse::Threads(rows),
                    ..
                }) => {
                    assert_eq!(rows[0].name, "original-thread");
                    response = true;
                }
                Some(DapEvent::Failed { message, .. }) => panic!("{message}"),
                None => break,
                _ => {}
            }
        }
        response
    });
    assert_eq!(
        outputs,
        (0..300)
            .map(|n| format!("{n} {}\n", "x".repeat(4000)))
            .collect::<Vec<_>>()
    );
    assert!(
        service.control_owned(&old, DapControl::Threads).is_err(),
        "initial context must not adopt stopped state"
    );
    service
        .control(DapControl::Evaluate {
            expression: "missing".into(),
            frame_id: None,
            context: "watch".into(),
        })
        .unwrap();
    wait(|| match service.try_event() {
        Some(DapEvent::Failed { message, .. }) => {
            panic!("request rejection destroyed the adapter: {message}")
        }
        Some(DapEvent::RequestFailed {
            command,
            request_seq,
            ..
        }) => {
            assert_eq!(command, "evaluate");
            assert!(request_seq > 0);
            true
        }
        Some(_) => false,
        None => false,
    });
    assert!(service.is_running());
    service.control(DapControl::Scopes { frame_id: 1 }).unwrap();
    wait(|| match service.try_event() {
        Some(DapEvent::RequestFailed { command, .. }) => {
            assert_eq!(command, "scopes");
            true
        }
        Some(DapEvent::Failed { message, .. }) => panic!("{message}"),
        _ => false,
    });
    assert!(service.is_running());
    let stopped = service.context().unwrap();
    service.stop().unwrap();
    wait(|| service.pending() == 0);
    let scoped_root = root.path().join("scoped");
    std::fs::create_dir(&scoped_root).unwrap();
    let identity = crate::files::RootIdentity::capture(&scoped_root).unwrap();
    std::fs::rename(&scoped_root, root.path().join("retained-old-root")).unwrap();
    std::fs::create_dir(&scoped_root).unwrap();
    let denied_script = root.path().join("denied.py");
    std::fs::write(
        &denied_script,
        "import pathlib\npathlib.Path('UNAUTHORIZED').write_text('spawned')\n",
    )
    .unwrap();
    let mut scoped = DapClient::new();
    drop(scoped.reservation().unwrap());
    assert_eq!(scoped.pending(), 0, "unused reservation leaked a resident");
    let reservation = scoped.reservation().unwrap();
    reservation
        .submit(
            launch(&scoped_root, &denied_script),
            Some(ExecutionScope {
                roots: vec![identity],
                paths: vec![scoped_root.clone()],
            }),
        )
        .unwrap();
    wait(|| match scoped.try_event() {
        Some(DapEvent::Failed { message, .. }) => {
            assert!(message.contains("filesystem identity changed"), "{message}");
            true
        }
        _ => false,
    });
    assert!(!scoped_root.join("UNAUTHORIZED").exists());
    scoped.stop().unwrap();
    wait(|| scoped.pending() == 0);
    std::fs::write(
        &script,
        "import pathlib,time\npathlib.Path('nonreading').write_text('ready')\ntime.sleep(60)\n",
    )
    .unwrap();
    service
        .start_absolute(launch(root.path(), &script))
        .unwrap();
    wait(|| root.path().join("nonreading").exists());
    assert!(
        service
            .control_owned(&stopped, DapControl::Threads)
            .is_err()
    );
    service
        .control(DapControl::Evaluate {
            expression: "x".repeat(64 * 1024),
            frame_id: None,
            context: "repl".into(),
        })
        .unwrap();
    let (stopped_tx, stopped_rx) = std::sync::mpsc::sync_channel(1);
    let stopper = thread::spawn(move || {
        service.stop().unwrap();
        stopped_tx.send(service).unwrap();
    });
    let mut service = stopped_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("foreground stop joined the nonreading adapter");
    stopper.join().unwrap();
    wait(|| service.pending() == 0);
    service.begin_shutdown();
    assert!(
        service
            .start_absolute(launch(root.path(), &script))
            .is_err()
    );
    // Shared framing must reject before consuming an unbounded newline-free header.
    let bytes = vec![b'x'; 1024 * 1024];
    let mut reader = std::io::Cursor::new(bytes);
    assert!(read_content_length_message(&mut reader).is_err());
    assert!(reader.position() <= 16385);
    terminal_reply_source_fixture();
    reverse_pressure_fixture();
}

fn reverse_pressure_fixture() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("reverse.py");
    let prefix = ADAPTER.split("while True:\n m=read()").next().unwrap();
    let body = r#"
received=[]
while True:
 m=read()
 if m is None: break
 if m.get('type')=='response':
  received.append(m);pathlib.Path('received.json').write_text(json.dumps(received))
  if len(received)==32: send({'type':'request','command':'runInTerminal','arguments':{'cwd':str(pathlib.Path.cwd()),'args':['owned'],'env':{}}})
  continue
 c=m['command']
 send({'type':'response','request_seq':m['seq'],'command':c,'success':True,'body':{}})
 if c=='launch':
  pathlib.Path('ready').write_text('ready')
  while not pathlib.Path('reverse-go').exists(): time.sleep(.002)
  for n in range(32): send({'type':'request','command':'runInTerminal','arguments':{'cwd':str(pathlib.Path.cwd()),'args':['owned'],'env':{}}})
"#;
    std::fs::write(&script, format!("{prefix}{body}")).unwrap();
    let mut service = DapClient::new();
    service
        .start_absolute(launch(root.path(), &script))
        .unwrap();
    wait(|| root.path().join("ready").exists());
    while service.try_event().is_some() {}
    service.writer_gate(true);
    service.control(DapControl::Scopes { frame_id: 1 }).unwrap();
    wait(|| service.writer_waiting());
    let reservation_deadline = Instant::now() + Duration::from_secs(3);
    for _ in 0..32 {
        loop {
            assert!(
                Instant::now() < reservation_deadline,
                "gated writer reservation deadline"
            );
            match service.control(DapControl::Scopes { frame_id: 1 }) {
                Ok(()) => break,
                Err(DapError::Full) => thread::sleep(Duration::from_millis(2)),
                Err(e) => panic!("{e}"),
            }
        }
    }
    wait(|| service.writes_queued() == 32);
    std::fs::write(root.path().join("reverse-go"), "go").unwrap();
    let mut replies = Vec::new();
    let mut sequences = Vec::new();
    wait(|| {
        for _ in 0..32 {
            match service.try_event() {
                Some(DapEvent::RunInTerminal {
                    request_seq, reply, ..
                }) => {
                    sequences.push(request_seq);
                    let reply = reply.claim();
                    reply.respond(true, None).unwrap();
                    replies.push(reply);
                }
                Some(DapEvent::Failed { message, .. }) => panic!("{message}"),
                None => break,
                _ => {}
            }
        }
        replies.len() == 32 && service.reverse_retries() > 0
    });
    assert!(
        replies
            .iter()
            .all(|r| r.outcome() == DapReplyOutcome::Ready)
    );
    // A retried reply counts itself for a moment before the full queue refuses it.
    wait(|| service.writes_queued() == 32);
    // This is an explicit gate BEFORE write, not a claimed blocked syscall.
    service.writer_gate(false);
    let mut late = None;
    wait(|| {
        for _ in 0..32 {
            match service.try_event() {
                Some(DapEvent::RunInTerminal {
                    request_seq, reply, ..
                }) => late = Some((request_seq, reply.claim())),
                Some(DapEvent::Failed { message, .. }) => panic!("{message}"),
                None => break,
                _ => {}
            }
        }
        replies
            .iter()
            .all(|r| r.outcome() == DapReplyOutcome::Written)
            && late.is_some()
    });
    let received: Vec<Value> =
        serde_json::from_slice(&std::fs::read(root.path().join("received.json")).unwrap()).unwrap();
    assert_eq!(received.len(), 32);
    assert_eq!(
        received
            .iter()
            .map(|v| v["request_seq"].as_i64().unwrap())
            .collect::<std::collections::BTreeSet<_>>(),
        sequences.iter().copied().collect()
    );
    assert!(received.iter().all(|v| v["success"] == true));
    assert!(
        received
            .windows(2)
            .all(|v| v[0]["seq"].as_i64() < v[1]["seq"].as_i64()),
        "reverse wire sequence lost enqueue ordering"
    );
    assert!(
        received
            .iter()
            .all(|v| v.get("message").is_none() && v["body"].get("processId").is_none()),
        "absent optional fields serialized as invalid nulls"
    );
    let (old_sequence, old_reply) = late.unwrap();
    service.stop().unwrap();
    wait(|| service.pending() == 0);
    assert_eq!(old_reply.outcome(), DapReplyOutcome::Unknown);
    // Force an equal reverse sequence in an actual same-name replacement actor.
    std::fs::remove_file(root.path().join("ready")).unwrap();
    let replacement_body = body.replace("  pathlib.Path('ready').write_text('ready')", &format!("  for n in range({}): send({{'type':'event','event':'output','body':{{'output':'warmup'}}}})\n  pathlib.Path('ready').write_text('ready')", old_sequence - 3));
    std::fs::write(&script, format!("{prefix}{replacement_body}")).unwrap();
    service
        .start_absolute(launch(root.path(), &script))
        .unwrap();
    let mut replacement = None;
    wait(|| match service.try_event() {
        Some(DapEvent::RunInTerminal {
            request_seq, reply, ..
        }) => {
            assert_eq!(request_seq, old_sequence);
            replacement = Some(reply.claim());
            true
        }
        Some(DapEvent::Failed { message, .. }) => panic!("{message}"),
        _ => false,
    });
    assert!(old_reply.respond(true, None).is_err());
    let replacement = replacement.unwrap();
    replacement.respond(true, None).unwrap();
    wait(|| {
        while service.try_event().is_some() {}
        replacement.outcome() == DapReplyOutcome::Written
    });
    service.stop().unwrap();
    wait(|| service.pending() == 0);
}

/// An adapter that LOGS every request it receives, so a test can count requests rather than read
/// code. The log is a file, exactly as the other fixtures record what they saw.
pub(crate) const LOGGING_ADAPTER: &str = r#"
import json,sys,pathlib
seq=0
log=pathlib.Path('requests.log')
def send(value):
 global seq
 seq+=1
 value['seq']=seq
 data=json.dumps(value).encode()
 sys.stdout.buffer.write(('Content-Length: %d\r\n\r\n'%len(data)).encode()+data)
 sys.stdout.buffer.flush()
def read():
 headers={}
 while True:
  line=sys.stdin.buffer.readline()
  if not line: return None
  if line in (b'\r\n',b'\n'): break
  k,v=line.decode().split(':',1); headers[k.lower()]=v.strip()
 return json.loads(sys.stdin.buffer.read(int(headers['content-length'])))
while True:
 m=read()
 if m is None: break
 if m.get('type')!='request': continue
 c=m['command']
 args=m.get('arguments',{})
 ref=args.get('variablesReference', args.get('frameId'))
 with log.open('a') as f: f.write('%s %s\n'%(c, ref))
 body={}
 if c=='initialize': body={'supportsConfigurationDoneRequest':True,'supportsEvaluateForHovers':True}
 if c=='threads': body={'threads':[{'id':1,'name':'logging-thread'}]}
 if c=='stackTrace': body={'stackFrames':[{'id':1,'name':'logging-frame','line':1,'column':1}]}
 if c=='configurationDone': body={}
 if c=='scopes': body={'scopes':[{'name':'locals','variablesReference':42,'expensive':False}]}
 if c=='variables':
  if ref==42: body={'variables':[{'name':'child','value':'1','variablesReference':77}]}
  if ref==77: body={'variables':[{'name':'grandchild','value':'2','variablesReference':0}]}
 send({'type':'response','request_seq':m['seq'],'command':c,'success':True,'body':body})
 if c in ('launch','attach'): send({'type':'event','event':'stopped','body':{'reason':'fixture','threadId':1}})
"#;
fn request_log(root: &Path) -> Vec<String> {
    std::fs::read_to_string(root.join("requests.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}
fn variables_requests(root: &Path) -> Vec<String> {
    request_log(root)
        .into_iter()
        .filter(|line| line.starts_with("variables "))
        .collect()
}
fn control_and_wait(service: &mut DapClient, control: DapControl, expect: &str) {
    service.control(control).unwrap();
    wait(|| {
        while let Some(event) = service.try_event() {
            match event {
                DapEvent::Response { response, .. } => {
                    let name = match response {
                        DapResponse::Scopes(_) => "scopes",
                        DapResponse::Variables(_) => "variables",
                        _ => "other",
                    };
                    if name == expect {
                        return true;
                    }
                }
                DapEvent::Failed { message, .. } => panic!("adapter failed: {message}"),
                _ => {}
            }
        }
        false
    });
}

#[test]
fn child_requests_happen_only_when_a_row_is_expanded() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("logging.py");
    std::fs::write(&script, LOGGING_ADAPTER).unwrap();
    let mut service = DapClient::new();
    service
        .start_absolute(launch(root.path(), &script))
        .unwrap();
    wait(|| matches!(service.try_event(), Some(DapEvent::Stopped { .. })));

    // OPENING A VIEW requests the scopes and STOPS THERE: the child request for the scope's
    // variables is NOT made while the list is built.
    control_and_wait(&mut service, DapControl::Scopes { frame_id: 1 }, "scopes");
    assert!(
        request_log(root.path())
            .iter()
            .any(|line| line.starts_with("scopes ")),
        "the scopes request really reached the adapter: {:?}",
        request_log(root.path())
    );
    assert_eq!(
        variables_requests(root.path()).len(),
        0,
        "building the list must not request children: {:?}",
        request_log(root.path())
    );

    // EXPANDING EXACTLY ONE ROW makes EXACTLY ONE child request, for THAT reference.
    control_and_wait(
        &mut service,
        DapControl::Variables {
            reference: 42,
            start: 0,
            count: 1000,
        },
        "variables",
    );
    assert_eq!(
        variables_requests(root.path()),
        vec!["variables 42".to_owned()],
        "one expansion, one request, for the reference that was expanded"
    );
    // And that child did NOT drag its own children with it.
    assert!(
        !request_log(root.path())
            .iter()
            .any(|line| line.contains("77")),
        "expanding a parent does not request the grandchild: {:?}",
        request_log(root.path())
    );
}

#[test]
fn a_declared_attach_session_sends_attach_and_needs_no_program() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("logging.py");
    std::fs::write(&script, LOGGING_ADAPTER).unwrap();
    let mut service = DapClient::new();
    let mut configuration = launch(root.path(), &script);
    // Declared as an ATTACH: the configuration says so, and there is no program to run.
    configuration.request = "attach".into();
    configuration.request_arguments = serde_json::json!({});
    service.start_absolute(configuration).unwrap();
    wait(|| matches!(service.try_event(), Some(DapEvent::Stopped { .. })));

    let log = request_log(root.path());
    assert!(
        log.iter().any(|line| line.starts_with("attach")),
        "an attach configuration really sends `attach`: {log:?}"
    );
    assert!(
        !log.iter().any(|line| line.starts_with("launch")),
        "and NOT `launch`, so the two are not the same request wearing different names: {log:?}"
    );
}

#[cfg(unix)]
#[test]
fn the_bridge_reads_one_whole_message_at_a_time() {
    let first = "Content-Length: 2\r\n\r\n{}";
    let mut input = std::io::Cursor::new(format!("{first}Content-Length: 1\r\n\r\n1").into_bytes());
    let frame = read_adapter_frame(&mut input).unwrap();
    assert_eq!(frame, first.as_bytes());
    assert_eq!(frame_body_length(&frame), 2);
    assert_eq!(read_adapter_frame(&mut input).unwrap().last(), Some(&b'1'));
    assert!(read_adapter_frame(&mut input).is_err());
}

#[test]
fn continued_events_only_invalidate_the_affected_stopped_thread() {
    let prefix = ADAPTER.split("while True:\n m=read()").next().unwrap();
    for (continued, preserved) in [
        (json!({"threadId":1,"allThreadsContinued":false}), true),
        (json!({"threadId":2,"allThreadsContinued":false}), false),
        (json!({"threadId":1,"allThreadsContinued":true}), false),
        (json!({"threadId":1}), false),
    ] {
        let body = format!(
            r#"
continued=json.loads('{continued}')
while True:
 m=read()
 if m is None: break
 c=m['command']; body={{}}
 if c=='threads': body={{'threads':[{{'id':2,'name':'stopped'}},{{'id':1,'name':'other'}}]}}
 if c=='evaluate':
  send({{'type':'event','event':'continued','body':continued}})
  body={{'result':'retained'}}
 send({{'type':'response','request_seq':m['seq'],'command':c,'success':True,'body':body}})
 if c=='launch': send({{'type':'event','event':'stopped','body':{{'reason':'entry','threadId':2}}}})
 if c=='evaluate': send({{'type':'event','event':'output','body':{{'output':'done'}}}})
"#
        );
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("continued.py");
        std::fs::write(&script, format!("{prefix}\n{body}")).unwrap();
        let mut service = DapClient::new();
        service
            .start_absolute(launch(root.path(), &script))
            .unwrap();
        wait(|| {
            matches!(
                service.try_event(),
                Some(DapEvent::Response {
                    response: DapResponse::Threads(_),
                    ..
                })
            )
        });
        let stopped = service.context().unwrap();
        service
            .control_owned(
                &stopped,
                DapControl::Evaluate {
                    expression: "value".into(),
                    frame_id: Some(2),
                    context: "watch".into(),
                },
            )
            .unwrap();
        let mut evaluated = false;
        let mut running = false;
        wait(|| match service.try_event() {
            Some(DapEvent::Response {
                response: DapResponse::Evaluate(_),
                ..
            }) => {
                evaluated = true;
                false
            }
            Some(DapEvent::Running { .. }) => {
                running = true;
                false
            }
            Some(DapEvent::Output { output, .. }) => {
                assert_eq!(output, "done");
                true
            }
            Some(DapEvent::Failed { message, .. }) => panic!("{message}"),
            _ => false,
        });
        assert_eq!(service.current(&stopped), preserved);
        assert_eq!(evaluated, preserved);
        assert_eq!(running, !preserved);
        service.stop().unwrap();
    }
}

#[test]
fn rejected_resumes_preserve_stopped_requests_and_the_selected_thread() {
    let prefix = ADAPTER.split("while True:\n m=read()").next().unwrap();
    let body = r#"
evaluation=None
while True:
 m=read()
 if m is None: break
 c=m['command']; body={}
 if c=='threads': body={'threads':[{'id':1,'name':'other'},{'id':2,'name':'stopped'}]}
 if c=='evaluate': evaluation=m;continue
 if c in ('stepIn','next'):
  assert m['arguments']['threadId']==2, 'lost the stopped thread'
  send({'type':'response','request_seq':m['seq'],'command':c,'success':False,'message':'still paused'})
  if evaluation:
   send({'type':'response','request_seq':evaluation['seq'],'command':'evaluate','success':True,'body':{'result':'retained','variablesReference':0}})
   evaluation=None
  continue
 send({'type':'response','request_seq':m['seq'],'command':c,'success':True,'body':body})
 if c=='launch': send({'type':'event','event':'stopped','body':{'reason':'entry','threadId':2}})
"#;
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("resume.py");
    std::fs::write(&script, format!("{prefix}\n{body}")).unwrap();
    let mut service = DapClient::new();
    service
        .start_absolute(launch(root.path(), &script))
        .unwrap();
    wait(|| {
        matches!(
            service.try_event(),
            Some(DapEvent::Response {
                response: DapResponse::Threads(_),
                ..
            })
        )
    });
    let stopped = service.context().unwrap();
    service
        .control(DapControl::Evaluate {
            expression: "value".into(),
            frame_id: Some(2),
            context: "watch".into(),
        })
        .unwrap();
    service.control(DapControl::StepIn).unwrap();
    let mut rejected = false;
    let mut evaluated = false;
    wait(|| {
        match service.try_event() {
            Some(DapEvent::RequestFailed { command, .. }) => {
                assert_eq!(command, "stepIn");
                rejected = true;
            }
            Some(DapEvent::Response {
                response: DapResponse::Evaluate(value),
                ..
            }) => {
                assert_eq!(value.value, "retained");
                evaluated = true;
            }
            Some(DapEvent::Failed { message, .. }) => panic!("{message}"),
            _ => {}
        }
        rejected && evaluated
    });
    assert!(service.current(&stopped));
    service.control_owned(&stopped, DapControl::Next).unwrap();
    wait(
        || matches!(service.try_event(), Some(DapEvent::RequestFailed { command, .. }) if command == "next"),
    );
    service.stop().unwrap();
}

#[test]
fn failed_pause_thread_lookup_cannot_pause_a_later_thread_list() {
    let prefix = ADAPTER.split("while True:\n m=read()").next().unwrap();
    let body = r#"
threads=0
while True:
 m=read()
 if m is None: break
 c=m['command']; body={}
 assert c!='pause', 'a failed pause survived its lookup'
 if c=='threads':
  threads+=1
  if threads==1:
   send({'type':'response','request_seq':m['seq'],'command':c,'success':False,'message':'no threads yet'});continue
  body={'threads':[{'id':2,'name':'later'}]}
 send({'type':'response','request_seq':m['seq'],'command':c,'success':True,'body':body})
 if c=='disconnect': pathlib.Path('no-stale-pause').write_text('done');break
"#;
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("pause.py");
    std::fs::write(&script, format!("{prefix}\n{body}")).unwrap();
    let mut service = DapClient::new();
    service
        .start_absolute(launch(root.path(), &script))
        .unwrap();
    wait(|| matches!(service.try_event(), Some(DapEvent::Running { .. })));
    let source = service.context().unwrap();
    let id = service.control_tracked(&source, DapControl::Pause).unwrap();
    wait(|| match service.try_event() {
        Some(DapEvent::RequestFailed {
            request_id,
            request: Some(DapControl::Pause),
            command,
            ..
        }) => {
            assert_eq!(request_id, id);
            assert_eq!(command, "pause");
            true
        }
        Some(DapEvent::Failed { message, .. }) => panic!("{message}"),
        _ => false,
    });
    service.control(DapControl::Threads).unwrap();
    wait(|| {
        matches!(
            service.try_event(),
            Some(DapEvent::Response {
                response: DapResponse::Threads(_),
                ..
            })
        )
    });
    service
        .control(DapControl::Disconnect {
            terminate_debuggee: true,
        })
        .unwrap();
    wait(|| {
        if let Some(DapEvent::Failed { message, .. }) = service.try_event() {
            panic!("{message}");
        }
        root.path().join("no-stale-pause").exists()
    });
    service.stop().unwrap();
}

#[test]
fn obsolete_breakpoint_responses_do_not_replace_current_adapter_ids() {
    let prefix = ADAPTER.split("while True:\n m=read()").next().unwrap();
    let body = r#"
requests=[]
while True:
 m=read()
 if m is None: break
 c=m['command']; body={}
 if c=='initialize': body={'supportsConfigurationDoneRequest':True}
 if c=='setBreakpoints':
  requests.append(m)
  if len(requests)<2: continue
  for item in reversed(requests):
   line=item['arguments']['breakpoints'][0]['line']
   send({'type':'response','request_seq':item['seq'],'command':c,'success':True,'body':{'breakpoints':[{'id':line,'verified':True,'line':line}]}})
  for line in [20,10]: send({'type':'event','event':'breakpoint','body':{'reason':'changed','breakpoint':{'id':line,'verified':True,'line':line}}})
  send({'type':'event','event':'output','body':{'output':'done'}})
  continue
 send({'type':'response','request_seq':m['seq'],'command':c,'success':True,'body':body})
 if c=='launch': send({'type':'event','event':'initialized','body':{}})
 if c=='configurationDone': send({'type':'event','event':'output','body':{'output':'ready'}})
"#;
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("breakpoint-order.py");
    std::fs::write(&script, format!("{prefix}\n{body}")).unwrap();
    let mut service = DapClient::new();
    service
        .start_absolute(launch(root.path(), &script))
        .unwrap();
    wait(
        || matches!(service.try_event(), Some(DapEvent::Output { output, .. }) if output == "ready"),
    );
    let path = root.path().join("main.rs");
    for line in [10, 20] {
        service
            .control(DapControl::SetBreakpoints {
                path: path.clone(),
                breakpoints: vec![DapSourceBreakpoint {
                    path: path.clone(),
                    line,
                    column: None,
                    condition: None,
                    hit_condition: None,
                    log_message: None,
                }],
            })
            .unwrap();
    }
    let mut changed = Vec::new();
    wait(|| match service.try_event() {
        Some(DapEvent::Response {
            response: DapResponse::BreakpointChanged { requested, .. },
            ..
        }) => {
            changed.push(requested);
            false
        }
        Some(DapEvent::Output { output, .. }) => output == "done",
        Some(DapEvent::Failed { message, .. }) => panic!("{message}"),
        _ => false,
    });
    assert_eq!(changed, vec![20]);
    service.stop().unwrap();
}

#[test]
fn responses_keep_request_context_and_breakpoints_survive_execution_changes() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("correlation.py");
    let prefix = LOGGING_ADAPTER
        .split("while True:\n m=read()")
        .next()
        .unwrap();
    let body = r#"
evaluations=[]
while True:
 m=read()
 if m is None: break
 c=m.get('command');a=m.get('arguments',{});body={}
 if c=='initialize':
  assert a['supportsVariablePaging'] is True
  body={'supportTerminateDebuggee':True}
 if c=='threads': body={'threads':[{'id':42,'name':'main'}]}
 if c=='pause': assert a['threadId']==42
 if c=='evaluate':
  evaluations.append(m)
  if len(evaluations)<2: continue
  for item in reversed(evaluations): send({'type':'response','request_seq':item['seq'],'command':'evaluate','success':True,'body':{'result':item['arguments']['expression']+'-value','variablesReference':0}})
  evaluations=[];continue
 if c=='setBreakpoints':
  send({'type':'event','event':'stopped','body':{'reason':'breakpoint','threadId':42}})
  body={'breakpoints':[{'id':7,'verified':False,'line':20}]}
 if c=='variables': body={'variables':[{'name':'child','value':'current','variablesReference':7}]}
 send({'type':'response','request_seq':m['seq'],'command':c,'success':True,'body':body})
 if c=='launch': send({'type':'event','event':'initialized','body':{}})
 if c=='setBreakpoints': send({'type':'event','event':'breakpoint','body':{'reason':'changed','breakpoint':{'id':7,'verified':True,'line':21}}})
 if c=='disconnect': break
"#;
    std::fs::write(&script, format!("{prefix}{body}")).unwrap();
    let mut service = DapClient::new();
    service
        .start_absolute(launch(root.path(), &script))
        .unwrap();
    wait(|| matches!(service.try_event(), Some(DapEvent::Running { .. })));
    service.control(DapControl::Pause).unwrap();
    wait(|| {
        matches!(
            service.try_event(),
            Some(DapEvent::Response {
                response: DapResponse::Threads(_),
                ..
            })
        )
    });
    for expression in ["a", "b"] {
        service
            .control(DapControl::Evaluate {
                expression: expression.into(),
                frame_id: None,
                context: "watch".into(),
            })
            .unwrap();
    }
    let mut values = Vec::new();
    wait(|| {
        while let Some(event) = service.try_event() {
            match event {
                DapEvent::Response {
                    request: Some(DapControl::Evaluate { expression, .. }),
                    response: DapResponse::Evaluate(value),
                    ..
                } => values.push((expression, value.value)),
                DapEvent::Failed { message, .. } => panic!("{message}"),
                _ => {}
            }
        }
        values.len() == 2
    });
    assert_eq!(
        values,
        [
            ("b".into(), "b-value".into()),
            ("a".into(), "a-value".into())
        ]
    );
    let path = root.path().join("main.rs");
    service
        .control(DapControl::SetBreakpoints {
            path: path.clone(),
            breakpoints: vec![DapSourceBreakpoint {
                path: path.clone(),
                line: 20,
                column: None,
                condition: None,
                hit_condition: None,
                log_message: None,
            }],
        })
        .unwrap();
    let mut acknowledged = false;
    let mut changed = false;
    wait(|| {
        while let Some(event) = service.try_event() {
            match event {
                DapEvent::Response {
                    response: DapResponse::Breakpoints(_),
                    request: Some(DapControl::SetBreakpoints { breakpoints, .. }),
                    ..
                } => {
                    assert_eq!(breakpoints[0].line, 20);
                    acknowledged = true;
                }
                DapEvent::Response {
                    response:
                        DapResponse::BreakpointChanged {
                            path: actual,
                            requested: 20,
                            verified: true,
                            line: Some(21),
                            ..
                        },
                    ..
                } => {
                    assert_eq!(actual, path);
                    changed = true;
                }
                DapEvent::Failed { message, .. } => panic!("{message}"),
                _ => {}
            }
        }
        acknowledged && changed
    });
    service
        .control(DapControl::Variables {
            reference: 8,
            start: 200,
            count: 200,
        })
        .unwrap();
    wait(|| {
        matches!(
            service.try_event(),
            Some(DapEvent::Response {
                request: Some(DapControl::Variables {
                    reference: 8,
                    start: 200,
                    count: 200
                }),
                response: DapResponse::Variables(_),
                ..
            })
        )
    });
    service
        .control(DapControl::Disconnect {
            terminate_debuggee: false,
        })
        .unwrap();
    wait(|| {
        matches!(
            service.try_event(),
            Some(DapEvent::Response {
                response: DapResponse::Disconnected {
                    terminate_debuggee: false
                },
                ..
            })
        )
    });
    wait(|| service.pending() == 0);
    assert!(
        service.try_event().is_none(),
        "disconnect must not synthesize a worker failure"
    );
    service.stop().unwrap();
}
#[test]
fn configuration_done_rejection_is_a_startup_failure() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("config.py");
    let prefix = LOGGING_ADAPTER
        .split("while True:\n m=read()")
        .next()
        .unwrap();
    let body = r#"
while True:
 m=read()
 if m is None: break
 c=m.get('command')
 body={'supportsConfigurationDoneRequest':True} if c=='initialize' else {}
 send({'type':'response','request_seq':m['seq'],'command':c,'success':c!='configurationDone','body':body,'message':'configuration rejected'})
 if c=='launch': send({'type':'event','event':'initialized','body':{}})
"#;
    std::fs::write(&script, format!("{prefix}{body}")).unwrap();
    let mut service = DapClient::new();
    service
        .start_absolute(launch(root.path(), &script))
        .unwrap();
    wait(|| match service.try_event() {
        Some(DapEvent::Failed { message, .. }) => {
            assert!(message.contains("configuration rejected"));
            true
        }
        _ => false,
    });
    service.stop().unwrap();
    wait(|| service.pending() == 0);
}
#[cfg(target_os = "linux")]
#[test]
fn abnormal_adapter_exit_kills_term_ignoring_descendants_and_releases_pipes() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("descendant.py");
    let prefix = LOGGING_ADAPTER
        .split("while True:\n m=read()")
        .next()
        .unwrap();
    std::fs::write(&script, format!("{prefix}\nimport subprocess,os\np=subprocess.Popen([sys.executable,'-c',\"import signal,time,pathlib; signal.signal(signal.SIGTERM,signal.SIG_IGN); pathlib.Path('helper-ready').touch(); time.sleep(30)\"])\npathlib.Path('helper-pid').write_text(str(p.pid))\nwhile not pathlib.Path('helper-ready').exists(): time.sleep(.002)\nos._exit(1)\n")).unwrap();
    let mut service = DapClient::new();
    service
        .start_absolute(launch(root.path(), &script))
        .unwrap();
    wait(|| root.path().join("helper-pid").exists());
    wait(|| {
        while service.try_event().is_some() {}
        service.pending() == 0
    });
    let pid = std::fs::read_to_string(root.path().join("helper-pid")).unwrap();
    wait(|| {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .map_or(true, |stat| stat.split_whitespace().nth(2) == Some("Z"))
    });
}

const SERVER_ADAPTER: &str = r#"
import socket,threading,json,sys,pathlib
server=socket.socket()
server.bind(('127.0.0.1',int(sys.argv[1])))
server.listen(4)
lock=threading.Lock()
def serve(sock,child):
 stream=sock.makefile('rwb',buffering=0)
 seq=0
 def send(value):
  nonlocal seq
  seq+=1
  value['seq']=seq
  data=json.dumps(value).encode()
  stream.write(('Content-Length: %d\r\n\r\n'%len(data)).encode()+data)
 while True:
  headers={}
  while True:
   line=stream.readline()
   if not line: return
   if line in (b'\r\n',b'\n'): break
   k,v=line.decode().split(':',1);headers[k.lower()]=v.strip()
  m=json.loads(stream.read(int(headers['content-length'])))
  with lock:
   with pathlib.Path('server.log').open('a') as log: log.write(json.dumps({'child':child,'message':m})+'\n')
  if m.get('type')!='request': continue
  c=m['command'];body={}
  if c=='initialize': body={'supportsConfigurationDoneRequest':True,'supportTerminateDebuggee':True}
  if c=='setBreakpoints': body={'breakpoints':[{'verified':True,'line':b['line']} for b in m['arguments']['breakpoints']]}
  if c=='threads': body={'threads':[{'id':7,'name':'node-child'}]}
  if c=='evaluate': body={'result':'42','variablesReference':0}
  send({'type':'response','request_seq':m['seq'],'command':c,'success':True,'body':body})
  if c=='initialize': send({'type':'event','event':'initialized'})
  if c=='configurationDone':
   if child: send({'type':'event','event':'stopped','body':{'reason':'breakpoint','threadId':7}})
   else: send({'type':'request','command':'startDebugging','arguments':{'request':'attach','configuration':{'type':'pwa-node','__pendingTargetId':'fixture-target'}}})
  if c=='disconnect':
   # Close after Dust does: Windows drops unread data when a peer resets the connection.
   try:
    while stream.read(4096): pass
   except OSError: pass
   return
root,_=server.accept()
threading.Thread(target=serve,args=(root,False),daemon=True).start()
child,_=server.accept()
serve(child,True)
"#;

#[test]
fn server_child_session_keeps_breakpoints_and_routes_controls_to_the_child() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("server.py");
    std::fs::write(&script, SERVER_ADAPTER).unwrap();
    let mut configuration = launch(root.path(), &script);
    configuration.arguments.push("{listen_port}".into());
    let source = root.path().join("demo.js");
    configuration.breakpoints = vec![(
        source.clone(),
        vec![DapSourceBreakpoint {
            path: source,
            line: 3,
            column: None,
            condition: None,
            hit_condition: None,
            log_message: None,
        }],
    )];
    let mut service = DapClient::new();
    service.start_absolute(configuration).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut stopped = false;
    let mut verified = 0;
    while !stopped || verified < 2 {
        match service.try_event() {
            Some(DapEvent::Stopped {
                thread_id: Some(7), ..
            }) => stopped = true,
            Some(DapEvent::Response {
                response: DapResponse::Breakpoints(rows),
                ..
            }) => {
                assert!(rows[0].0);
                verified += 1;
            }
            Some(DapEvent::Failed { message, .. }) => panic!("server adapter failed: {message}"),
            _ => {}
        }
        assert!(Instant::now() < deadline, "child did not stop");
        thread::sleep(Duration::from_millis(2));
    }
    service
        .control(DapControl::Evaluate {
            expression: "6*7".into(),
            frame_id: None,
            context: "repl".into(),
        })
        .unwrap();
    let mut evaluated = false;
    while !evaluated {
        match service.try_event() {
            Some(DapEvent::Response {
                response: DapResponse::Evaluate(value),
                ..
            }) => {
                assert_eq!(value.value, "42");
                evaluated = true;
            }
            Some(DapEvent::Failed { message, .. }) => panic!("server adapter failed: {message}"),
            _ => {}
        }
        assert!(Instant::now() < deadline, "child evaluation timed out");
        thread::sleep(Duration::from_millis(2));
    }
    service
        .control(DapControl::Disconnect {
            terminate_debuggee: true,
        })
        .unwrap();
    let mut disconnected = false;
    while !disconnected {
        match service.try_event() {
            Some(DapEvent::Response {
                response:
                    DapResponse::Disconnected {
                        terminate_debuggee: true,
                    },
                ..
            }) => disconnected = true,
            Some(DapEvent::Failed { message, .. }) => panic!("server adapter failed: {message}"),
            _ => {}
        }
        assert!(Instant::now() < deadline, "child disconnect timed out");
        thread::sleep(Duration::from_millis(2));
    }
    let log = std::fs::read_to_string(root.path().join("server.log")).unwrap();
    let log = log
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(log.iter().any(|row| row["child"] == false
        && row["message"]["command"] == "startDebugging"
        && row["message"]["success"] == true));
    assert!(log.iter().any(|row| row["child"] == true
        && row["message"]["command"] == "attach"
        && row["message"]["arguments"]["__pendingTargetId"] == "fixture-target"));
    assert!(
        log.iter()
            .any(|row| row["child"] == true && row["message"]["command"] == "evaluate")
    );
    let _ = service.stop();
}

#[test]
fn server_child_session_rejects_missing_configuration() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("server.py");
    std::fs::write(
        &script,
        SERVER_ADAPTER.replace(
            "'configuration':{'type':'pwa-node','__pendingTargetId':'fixture-target'}",
            "'configuration':None",
        ),
    )
    .unwrap();
    let mut configuration = launch(root.path(), &script);
    configuration.arguments.push("{listen_port}".into());
    let mut service = DapClient::new();
    service.start_absolute(configuration).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(DapEvent::Failed { message, .. }) = service.try_event() {
            assert!(message.contains("child configuration missing"), "{message}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "invalid child configuration was not rejected"
        );
        thread::sleep(Duration::from_millis(2));
    }
    let _ = service.stop();
}

#[test]
fn stopping_a_server_launch_cancels_the_connection_wait() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("server.py");
    std::fs::write(&script, "import time\ntime.sleep(60)\n").unwrap();
    let mut configuration = launch(root.path(), &script);
    configuration.arguments.push("{listen_port}".into());
    let mut service = DapClient::new();
    service.start_absolute(configuration).unwrap();
    thread::sleep(Duration::from_millis(50));
    service.stop().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while service.pending() != 0 {
        service.try_event();
        assert!(
            Instant::now() < deadline,
            "cancelled connection wait retained workers"
        );
        thread::sleep(Duration::from_millis(5));
    }
}
