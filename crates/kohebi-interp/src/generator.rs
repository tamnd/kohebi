//! Generators: a call that hands back a frame instead of running it.
//!
//! A `def` with a `yield` anywhere in it is a different kind of thing from a
//! `def` without one, and the difference is decided at compile time rather than
//! at the first `yield`. Calling it runs none of the body. It binds the
//! arguments into a frame, puts the frame in one of these, and gives that back,
//! and the body runs a piece at a time from there.
//!
//! The frame is the whole of the state. Registers, program counter and open
//! `try` regions all sit in it exactly as they did when the `yield` stopped, so
//! resuming is putting it back on the machine and carrying on from the
//! instruction after the one that stopped. Nothing about the body is compiled
//! differently for being in a generator, which is what keeps the interpreter
//! loop the same loop.
//!
//! ## What is not implemented
//!
//! `send`, `close` and `throw`, and `yield from`, which is written in terms of
//! them. All four are reached as attributes of the generator object, and an
//! attribute of anything that is not a class or an instance of one has nowhere
//! to be looked up yet. The machinery underneath them is here: a resume already
//! carries a value in and already knows which register the last `yield` wanted
//! it written to.

use std::any::Any;
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use kohebi_bc::code::Reg;
use kohebi_core::Native;

use crate::ready::Ready;
use crate::vm::Frame;

/// A suspended call.
pub struct Generator {
    /// The body, with its constants already built. Shared rather than borrowed
    /// because this outlives the call that made it, which is the whole point.
    ready: Rc<Ready>,
    state: RefCell<State>,
}

/// Where a generator is between two of its steps.
enum State {
    /// Not started, or stopped at a `yield`. The frame is everything it will
    /// need to carry on.
    Suspended {
        frame: Box<Frame>,
        /// The register the `yield` that stopped here wrote, so that a `send`
        /// has somewhere to put its value. `None` before the first step, where
        /// there is no `yield` to have wanted anything.
        resume: Option<Reg>,
    },
    /// Being stepped right now.
    ///
    /// A program can see this: a generator whose body asks for the next value
    /// of itself finds it here, and that is a `ValueError` rather than a
    /// deadlock or a second frame.
    Running,
    /// Over, whether it returned or raised. A finished generator is an empty
    /// iterator forever after, which is why this is a state and not a drop.
    Done,
}

impl Generator {
    /// A generator over this body, with its arguments already in the frame and
    /// none of it run.
    #[must_use]
    pub(crate) fn new(ready: Rc<Ready>, frame: Frame) -> Self {
        Generator {
            ready,
            state: RefCell::new(State::Suspended {
                frame: Box::new(frame),
                resume: None,
            }),
        }
    }

    /// The body, for the machine that is about to step it.
    pub(crate) fn ready(&self) -> &Rc<Ready> {
        &self.ready
    }

    /// Take the frame out to run it, leaving this generator marked as running.
    ///
    /// `None` means there is nothing to run: it has finished, or it is already
    /// running somewhere further down the stack. The two are told apart by
    /// [`Generator::is_running`], because only one of them is a mistake.
    pub(crate) fn start(&self) -> Option<(Box<Frame>, Option<Reg>)> {
        let mut state = self.state.borrow_mut();
        match std::mem::replace(&mut *state, State::Running) {
            State::Suspended { frame, resume } => Some((frame, resume)),
            // Put back what was there. The replace above has already written
            // `Running` over it, and a finished generator must not come back as
            // a running one.
            other => {
                *state = other;
                None
            }
        }
    }

    /// Put the frame back, stopped at the `yield` that wrote `resume`.
    pub(crate) fn suspend(&self, frame: Box<Frame>, resume: Option<Reg>) {
        *self.state.borrow_mut() = State::Suspended { frame, resume };
    }

    /// Mark it finished, which is what a `return` and a raise both do.
    pub(crate) fn finish(&self) {
        *self.state.borrow_mut() = State::Done;
    }

    /// Whether a step is in progress, which is how a generator that asks for
    /// itself is told apart from one that is simply over.
    pub(crate) fn is_running(&self) -> bool {
        matches!(*self.state.borrow(), State::Running)
    }
}

impl fmt::Debug for Generator {
    /// The name and the state, because the frame is a listing's worth of
    /// registers and nothing debugging a generator wants all of them.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match *self.state.borrow() {
            State::Suspended { .. } => "suspended",
            State::Running => "running",
            State::Done => "done",
        };
        write!(f, "<generator {} {state}>", self.ready.code().qualname)
    }
}

impl Native for Generator {
    fn type_name(&self) -> &str {
        "generator"
    }

    /// The qualified name, so a generator method reads as `C.f` and one defined
    /// inside a function as `outer.<locals>.g`, which is what CPython prints.
    fn repr(&self) -> String {
        format!(
            "<generator object {} at {:#x}>",
            self.ready.code().qualname,
            std::ptr::from_ref(self) as usize
        )
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
