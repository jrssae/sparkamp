//! A backend with no audio stack behind it.
//!
//! Everything the trait promises, recorded rather than performed, so a test can
//! read back exactly what crossed the seam and drive the two things a real
//! backend only does when hardware cooperates: reaching a running state, and
//! posting a transport event.
//!
//! This is what a test asks for when it does not want GStreamer. Before the
//! seam there was no way to ask: every test built a real pipeline, so the EQ
//! element had to be neutralised for all of them at once.

use std::collections::VecDeque;
use std::time::Duration;

use anyhow::Result;

use crate::engine::backend::{
    Amplitude, AnalysisTap, Applied, AudioBackend, Capabilities, EqCurve, MediaSource,
    Normalization, Timeline,
};
use crate::engine::{BusEvent, PlayerState};

pub struct NullBackend {
    eq: EqCurve,
    gain: Amplitude,
    normalization: Normalization,
    /// Set when [`Self::defer_reshape`] is on and a reshape was asked for while
    /// running, applied at the next `load` exactly as GStreamer's is.
    pending: Option<Normalization>,
    defer_reshape: bool,
    state: PlayerState,
    events: VecDeque<BusEvent>,
    #[allow(dead_code)]
    tap: AnalysisTap,
}

impl NullBackend {
    /// The curve currently installed, as the backend received it.
    pub fn eq(&self) -> EqCurve {
        self.eq
    }

    /// The last output level pushed across the seam.
    pub fn output_gain(&self) -> Amplitude {
        self.gain
    }

    /// The normalization in force, which is not the same as the last one
    /// requested when [`Self::defer_reshape`] is on.
    pub fn normalization(&self) -> Normalization {
        self.normalization
    }

    /// Put the backend in a state `Player` cannot drive it to on its own — a
    /// null backend has no pipeline that can fail to preroll, and a test that
    /// needs "running" should not have to lie to `Player` about it.
    pub fn force_state(&mut self, state: PlayerState) {
        self.state = state;
    }

    /// Queue an event for the next [`AudioBackend::poll_event`].
    pub fn post_event(&mut self, event: BusEvent) {
        self.events.push_back(event);
    }

    /// Behave like an adapter that can only reshape its audio path while
    /// stopped, which is GStreamer's relink rule and the reason `Applied`
    /// exists at all.
    pub fn defer_reshape(&mut self, defer: bool) {
        self.defer_reshape = defer;
    }
}

impl AudioBackend for NullBackend {
    fn open(tap: AnalysisTap) -> Result<Self> {
        Ok(NullBackend {
            eq: EqCurve::FLAT,
            gain: Amplitude::UNITY,
            normalization: Normalization {
                enabled: false,
                clip_protection: false,
                fallback_db: 0.0,
                album_mode: false,
            },
            pending: None,
            defer_reshape: false,
            state: PlayerState::Stopped,
            events: VecDeque::new(),
            tap,
        })
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            eq: true,
            spectrum: true,
            normalization: true,
        }
    }

    fn load(&mut self, _source: &MediaSource) -> Result<()> {
        if let Some(want) = self.pending.take() {
            self.normalization = want;
        }
        self.state = PlayerState::Stopped;
        Ok(())
    }

    fn set_state(&mut self, state: PlayerState) -> Result<()> {
        self.state = state;
        Ok(())
    }

    fn seek(&mut self, _to: Duration) -> Result<()> {
        Ok(())
    }

    fn timeline(&self) -> Timeline {
        Timeline::default()
    }

    fn poll_event(&mut self) -> Option<BusEvent> {
        self.events.pop_front()
    }

    fn set_output_gain(&mut self, gain: Amplitude) {
        self.gain = gain;
    }

    fn set_eq(&mut self, curve: &EqCurve) {
        self.eq = *curve;
    }

    fn set_normalization(&mut self, want: Normalization) -> Applied {
        let reshape = want.enabled != self.normalization.enabled
            || (want.enabled && want.clip_protection != self.normalization.clip_protection);
        if reshape && self.defer_reshape && self.state != PlayerState::Stopped {
            self.pending = Some(want);
            return Applied::AtNextLoad;
        }
        self.pending = None;
        self.normalization = want;
        Applied::Now
    }
}
