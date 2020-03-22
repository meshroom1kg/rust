use std::cell::RefCell;

use rustc_span::DUMMY_SP;

use crate::*;

/// Details of premature program termination.
pub enum TerminationInfo {
    Exit(i64),
    Abort(Option<String>),
    UnsupportedInIsolation(String),
    ExperimentalUb { msg: String, url: String }
}

/// Miri specific diagnostics
pub enum NonHaltingDiagnostic {
    PoppedTrackedPointerTag(Item),
    CreatedAlloc(AllocId),
}

/// Emit a custom diagnostic without going through the miri-engine machinery
pub fn report_error<'tcx, 'mir>(
    ecx: &InterpCx<'mir, 'tcx, Evaluator<'tcx>>,
    mut e: InterpErrorInfo<'tcx>,
) -> Option<i64> {
    use InterpError::*;

    e.print_backtrace();
    let (title, msg, helps) = match e.kind {
        MachineStop(info) => {
            let info = info.downcast_ref::<TerminationInfo>().expect("invalid MachineStop payload");
            use TerminationInfo::*;
            let (title, msg) = match info {
                Exit(code) => return Some(*code),
                Abort(None) =>
                    ("abnormal termination", format!("the evaluated program aborted execution")),
                Abort(Some(msg)) =>
                    ("abnormal termination", format!("the evaluated program aborted execution: {}", msg)),
                UnsupportedInIsolation(msg) =>
                    ("unsupported operation", format!("{}", msg)),
                ExperimentalUb { msg, .. } =>
                    ("Undefined Behavior", format!("{}", msg)),
            };
            let helps = match info {
                UnsupportedInIsolation(_) =>
                    vec![format!("pass the flag `-Zmiri-disable-isolation` to disable isolation")],
                ExperimentalUb { url, .. } =>
                    vec![
                        format!("this indicates a potential bug in the program: it performed an invalid operation, but the rules it violated are still experimental"),
                        format!("see {} for further information", url),
                    ],
                _ => vec![],
            };
            (title, msg, helps)
        }
        _ => {
            let (title, msg) = match e.kind {
                Unsupported(_) =>
                    ("unsupported operation", e.to_string()),
                UndefinedBehavior(_) =>
                    ("Undefined Behavior", e.to_string()),
                ResourceExhaustion(_) =>
                    ("resource exhaustion", e.to_string()),
                _ =>
                    bug!("This error should be impossible in Miri: {}", e),
            };
            let helps = match e.kind {
                Unsupported(UnsupportedOpInfo::NoMirFor(..)) =>
                    vec![format!("make sure to use a Miri sysroot, which you can prepare with `cargo miri setup`")],
                Unsupported(_) =>
                    vec![format!("this is likely not a bug in the program; it indicates that the program performed an operation that the interpreter does not support")],
                UndefinedBehavior(_) =>
                    vec![
                        format!("this indicates a bug in the program: it performed an invalid operation, and caused Undefined Behavior"),
                        format!("see https://doc.rust-lang.org/nightly/reference/behavior-considered-undefined.html for further information"),
                    ],
                _ => vec![],
            };
            (title, msg, helps)
        }
    };
    report_msg(ecx, &format!("{}: {}", title, msg), msg, &helps, true)
}

/// Report an error or note (depending on the `error` argument) at the current frame's current statement.
/// Also emits a full stacktrace of the interpreter stack.
fn report_msg<'tcx, 'mir>(
    ecx: &InterpCx<'mir, 'tcx, Evaluator<'tcx>>,
    title: &str,
    span_msg: String,
    helps: &[String],
    error: bool,
) -> Option<i64> {
    let span = if let Some(frame) = ecx.stack().last() {
        frame.current_source_info().unwrap().span
    } else {
        DUMMY_SP
    };
    let mut err = if error {
        ecx.tcx.sess.struct_span_err(span, title)
    } else {
        ecx.tcx.sess.diagnostic().span_note_diag(span, title)
    };
    err.span_label(span, span_msg);
    for help in helps {
        err.help(help);
    }
    // Add backtrace
    let frames = ecx.generate_stacktrace(None);
    // We iterate with indices because we need to look at the next frame (the caller).
    for idx in 0..frames.len() {
        let frame_info = &frames[idx];
        let call_site_is_local = frames
            .get(idx + 1)
            .map_or(false, |caller_info| caller_info.instance.def_id().is_local());
        if call_site_is_local {
            err.span_note(frame_info.call_site, &frame_info.to_string());
        } else {
            err.note(&frame_info.to_string());
        }
    }

    err.emit();

    for (i, frame) in ecx.stack().iter().enumerate() {
        trace!("-------------------");
        trace!("Frame {}", i);
        trace!("    return: {:?}", frame.return_place.map(|p| *p));
        for (i, local) in frame.locals.iter().enumerate() {
            trace!("    local {}: {:?}", i, local.value);
        }
    }
    // Let the reported error determine the return code.
    return None;
}

thread_local! {
    static DIAGNOSTICS: RefCell<Vec<NonHaltingDiagnostic>> = RefCell::new(Vec::new());
}

/// Schedule a diagnostic for emitting. This function works even if you have no `InterpCx` available.
/// The diagnostic will be emitted after the current interpreter step is finished.
pub fn register_diagnostic(e: NonHaltingDiagnostic) {
    DIAGNOSTICS.with(|diagnostics| diagnostics.borrow_mut().push(e));
}

impl<'mir, 'tcx> EvalContextExt<'mir, 'tcx> for crate::MiriEvalContext<'mir, 'tcx> {}
pub trait EvalContextExt<'mir, 'tcx: 'mir>: crate::MiriEvalContextExt<'mir, 'tcx> {
    /// Emit all diagnostics that were registed with `register_diagnostics`
    fn process_diagnostics(&self) {
        let this = self.eval_context_ref();
        DIAGNOSTICS.with(|diagnostics| {
            for e in diagnostics.borrow_mut().drain(..) {
                use NonHaltingDiagnostic::*;
                let msg = match e {
                    PoppedTrackedPointerTag(item) =>
                        format!("popped tracked tag for item {:?}", item),
                    CreatedAlloc(AllocId(id)) =>
                        format!("created allocation with id {}", id),
                };
                report_msg(this, "tracking was triggered", msg, &[], false);
            }
        });
    }
}
