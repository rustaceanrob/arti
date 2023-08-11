//! Utilities to track and compare times and timeouts
//!
//! Contains [`TrackingNow`], and variants.
//!
//! Each one records the current time,
//! and can be used to see if prospective timeouts have expired yet,
//! via the [`PartialOrd`] implementations.
//!
//! Each can be compared with a prospective wakeup time via a `.cmp()` method,
//! and via implementations of [`PartialOrd`] (including via `<` operators etc.)
//!
//! Each tracks every such comparison,
//! and can yield the earliest timeout that was asked about.
//!
//! Each has interior mutability,
//! which is necessary because `PartialOrd` (`<=` etc.) only passes immutable references.
//! Most are `Send`, none are `Sync`,
//! so use in thread-safe async code is somewhat restricted.
//! (Recommended use is to do all work influencing timeout calculations synchronously;
//! otherwise, in any case, you risk the time advancing mid-calculations.)
//!
//! `Clone` gives you a *copy*, not a handle onto the same tracker.
//! Comparisons done with the clone do not update the original.
//! (Exception: `TrackingInstantOffsetNow::clone`.)
//!
//! The types are:
//!
//!  * [`TrackingNow`]: tracks timeouts based on both [`SystemTime` and `Instant`],
//!  * [`TrackingSystemTimeNow`]: tracks timeouts based on [`SystemTime`]
//!  * [`TrackingInstantNow`]: tracks timeouts based on [`Instant`]
//!  * [`TrackingInstantOffsetNow`]: `InstantTrackingNow` but with an offset applied

#![allow(unreachable_pub)] // TODO - eventually we hope this will become pub, in another crate

use std::cell::Cell;
use std::cmp::Ordering;
use std::time::{Duration, Instant, SystemTime};

use derive_adhoc::{define_derive_adhoc, Adhoc};
use futures::{future, select_biased, FutureExt as _};
use itertools::chain;

use tor_rtcompat::{SleepProvider, SleepProviderExt as _};

//========== derive-adhoc macros, which must come first ==========

define_derive_adhoc! {
    /// Defines methods and types which are common to trackers for `Instant` and `SystemTime`
    SingleTimeoutTracker for struct, expect items =

    // type of the `now` field, ie the absolute time type
    ${define NOW $(
        ${when approx_equal($fname, now)}
        $ftype
    ) }

    // type that we track, ie the inner contents of the `Cell<Option<...>>`
    ${define TRACK ${tmeta(track)}}

    // TODO maybe some of this should be a trait?  But that would probably include
    // wait_for_earliest, which would be an async trait method and quite annoying.
    impl $ttype {
        /// Creates a new timeout tracker, given a value for the current time
        pub fn new(now: $NOW) -> Self {
            Self {
                now,
                earliest: None.into(),
            }
        }

        /// Creates a new timeout tracker from the current time as seen by a runtime
        pub fn now(r: &impl SleepProvider) -> Self {
            let now = r.${tmeta(from_runtime)}();
            Self::new(now)
        }

        /// Return the "current time" value in use
        ///
        /// If you do comparisons with this, they won't be tracked, obviously.
        pub fn get_now_untracked(&self) -> $NOW {
            self.now
        }

        /// Core of a tracked update: updates `earliest` with `maybe_earlier`
        fn update_inner(earliest: &Cell<Option<$TRACK>>, maybe_earlier: $TRACK) {
            earliest.set(chain!(
                earliest.take(),
                [maybe_earlier],
            ).min())
        }
    }
}

define_derive_adhoc! {
    /// Impls for `TrackingNow`, the combined tracker
    ///
    /// Defines just the methods which want to abstract over fields
    CombinedTimeoutTracker for struct, expect items =

    ${define NOW ${fmeta(now)}}

    impl $ttype {
        /// Creates a new combined timeout tracker, given values for the current time
        pub fn new( $(
            $fname: $NOW,
        ) ) -> $ttype {
            $ttype { $( $fname: $ftype::new($fname), ) }
        }

        /// Creates a new timeout tracker from the current times as seen by a runtime
        pub fn now(r: &impl SleepProvider) -> Self {
            $ttype { $(
                $fname: $ftype::now(r),
            ) }
        }

      $(
        #[doc = concat!("Access the specific timeout tracker for [`", stringify!($NOW), "`]")]
        pub fn $fname(&self) -> &$ftype {
            &self.$fname
        }
      )
    }

  $(
    define_PartialOrd_via_cmp! { $ttype, $NOW, .$fname }
  )
}

define_derive_adhoc! {
    /// Defines `wait_for_earliest`
    ///
    /// Combined into this macro mostly so we only have to write the docs once
    WaitForEarliest for struct, expect items =

    impl $ttype {
        /// Wait for the earliest timeout implied by any of the comparisons
        ///
        /// Waits until the earliest time at which any of the comparisons performed
        /// might change their answer.
        ///
        /// If there were no comparisons there are no timeouts, so we wait forever.
        pub async fn wait_for_earliest(self, runtime: &impl SleepProvider) {
            ${if tmeta(runtime_sleep) {
                // tracker for a single kind of time
                match self.earliest.into_inner() {
                    None => future::pending().await,
                    Some(earliest) => runtime.${tmeta(runtime_sleep)}(earliest).await,
                }
            } else {
                // combined tracker, wait for earliest of any kind of timeout
                select_biased! { $(
                    () = self.$fname.wait_for_earliest(runtime).fuse() => {},
                ) }
            }}
        }
    }
}

/// `impl PartialOrd<$NOW> for $ttype` in terms of `...$field.cmp()`
macro_rules! define_PartialOrd_via_cmp { {
    $ttype:ty, $NOW:ty, $( $field:tt )*
} => {
    /// Check if time `t` has been reached yet (and remember that we want to wake up then)
    ///
    /// Always returns `Some`.
    impl PartialEq<$NOW> for $ttype {
        fn eq(&self, t: &$NOW) -> bool {
            self $($field)* .cmp(*t) == Ordering::Equal
        }
    }

    /// Check if time `t` has been reached yet (and remember that we want to wake up then)
    ///
    /// Always returns `Some`.
    impl PartialOrd<$NOW> for $ttype {
        fn partial_cmp(&self, t: &$NOW) -> Option<std::cmp::Ordering> {
            Some(self $($field)* .cmp(*t))
        }
    }

    /// Check if we have reached time `t` yet (and remember that we want to wake up then)
    ///
    /// Always returns `Some`.
    impl PartialEq<$ttype> for $NOW {
        fn eq(&self, t: &$ttype) -> bool {
            t.eq(self)
        }
    }

    /// Check if we have reached time `t` yet (and remember that we want to wake up then)
    ///
    /// Always returns `Some`.
    impl PartialOrd<$ttype> for $NOW {
        fn partial_cmp(&self, t: &$ttype) -> Option<std::cmp::Ordering> {
            t.partial_cmp(self).map(|o| o.reverse())
        }
    }
} }

//========== data structures ==========

/// Utility to track timeouts based on [`SystemTime`] (wall clock time)
///
/// Represents the current `SystemTime` (from when it was created).
/// See the [module-level documentation](self) for the general overview.
///
/// To operate a timeout,
/// you should calculate the `SystemTime` at which you should time out,
/// and compare that future planned wakeup time with this `TrackingSystemTimeNow`
/// (via [`.cmp()`](Self::cmp) or inequality operators and [`PartialOrd`]).
#[derive(Clone, Debug, Adhoc)]
#[derive_adhoc(SingleTimeoutTracker, WaitForEarliest)]
#[adhoc(track = "SystemTime")]
#[adhoc(from_runtime = "wallclock", runtime_sleep = "sleep_until_wallclock")]
pub struct TrackingSystemTimeNow {
    /// Current time
    now: SystemTime,
    /// Earliest time at which we should wake up
    earliest: Cell<Option<SystemTime>>,
}

/// Earliest timeout at which an [`Instant`] based timeout should occur, as duration from now
///
/// The actual tracker, found via `TrackingInstantNow` or `TrackingInstantOffsetNow`
type InstantEarliest = Cell<Option<Duration>>;

/// Utility to track timeouts based on [`Instant`] (monotonic time)
///
/// Represents the current `Instant` (from when it was created).
/// See the [module-level documentation](self) for the general overview.
///
/// To calculate and check a timeout,
/// you can
/// calculate the future `Instant` at which you wish to wake up,
/// and compare it with a `TrackingInstantNow`,
/// via [`.cmp()`](Self::cmp) or inequality operators and [`PartialOrd`].
///
/// Or you can
/// use
/// [`.checked_sub()`](TrackingInstantNow::checked_sub)
/// to obtain a [`TrackingInstantOffsetNow`].
#[derive(Clone, Debug, Adhoc)]
#[derive_adhoc(SingleTimeoutTracker, WaitForEarliest)]
#[adhoc(track = "Duration")]
#[adhoc(from_runtime = "now", runtime_sleep = "sleep")]
pub struct TrackingInstantNow {
    /// Current time
    now: Instant,
    /// Duration until earliest time we should wake up
    earliest: InstantEarliest,
}

/// Current minus an offset, for [`Instant`]-based timeout checks
///
/// Returned by
/// [`TrackingNow::checked_sub()`]
/// and
/// [`TrackingInstantNow::checked_sub()`].
///
/// You can compare this with an interesting fixed `Instant`,
/// via [`.cmp()`](Self::cmp) or inequality operators and [`PartialOrd`].
///
/// Borrows from its parent `TrackingInstantNow`;
/// multiple different `TrackingInstantOffsetNow`'s can exist
/// for the same parent tracker,
/// and they'll all update it.
///
/// (There is no corresponding call for `SystemTime`;
/// see the [docs for `TrackingNow::checked_sub()`](TrackingNow::checked_sub)
/// for why.)
#[derive(Debug)]
pub struct TrackingInstantOffsetNow<'i> {
    /// Value to compare with
    threshold: Instant,
    /// Comparison tracker
    earliest: &'i InstantEarliest,
}

/// Timeout tracker that can handle both `Instant`s and `SystemTime`s
///
/// Internally, the two kinds of timeouts are tracked separately:
/// this contains a [`TrackingInstantNow`] and a [`TrackingSystemTimeNow`].
#[derive(Clone, Debug, Adhoc)]
#[derive_adhoc(CombinedTimeoutTracker, WaitForEarliest)]
pub struct TrackingNow {
    /// For `Instant`s
    #[adhoc(now = "Instant")]
    instant: TrackingInstantNow,
    /// For `SystemTime`s
    #[adhoc(now = "SystemTime")]
    system_time: TrackingSystemTimeNow,
}

//========== implementations, organised by theme ==========

//----- earliest accessor ----

impl TrackingSystemTimeNow {
    /// Return the earliest `SystemTime` with which this has been compared
    pub fn earliest(self) -> Option<SystemTime> {
        self.earliest.into_inner()
    }
}

impl TrackingInstantNow {
    /// Return the shortest `Duration` until any `Instant` with which this has been compared
    pub fn shortest(self) -> Option<Duration> {
        self.earliest.into_inner()
    }
}

//----- manual update functions ----

impl TrackingSystemTimeNow {
    /// Update the "earliest timeout" notion, to ensure it's at least as early as `t`
    ///
    /// (Equivalent to comparing with `t` but discarding the answer.)
    pub fn update(&self, t: SystemTime) {
        Self::update_inner(&self.earliest, t);
    }
}

impl TrackingInstantNow {
    /// Update the "earliest timeout" notion, to ensure it's at least as early as `t`
    ///
    /// Equivalent to comparing with `t` but discarding the answer.
    fn update_abs(&self, t: Instant) {
        self.update_rel(t.checked_duration_since(self.now).unwrap_or_default());
    }

    /// Update the "earliest timeout" notion, to ensure it's at no later than `d` from now
    fn update_rel(&self, d: Duration) {
        Self::update_inner(&self.earliest, d);
    }
}

//----- cmp and PartialOrd implementation ----

impl TrackingSystemTimeNow {
    /// Check if time `t` has been reached yet (and remember that we want to wake up then)
    ///
    /// Also available via [`PartialOrd`]
    fn cmp(&self, t: SystemTime) -> std::cmp::Ordering {
        Self::update_inner(&self.earliest, t);
        self.now.cmp(&t)
    }
}
define_PartialOrd_via_cmp! { TrackingSystemTimeNow, SystemTime, }

/// Check `t` against a now-based `threshold` (and remember for wakeup)
///
/// Common code for `TrackingInstantNow` and `TrackingInstantOffsetNow`'s
/// `cmp`.
fn instant_cmp(earliest: &InstantEarliest, threshold: Instant, t: Instant) -> Ordering {
    let Some(d) = t.checked_duration_since(threshold) else {
        earliest.set(Some(Duration::ZERO));
        return Ordering::Less;
    };

    TrackingInstantNow::update_inner(earliest, d);
    d.cmp(&Duration::ZERO)
}

impl TrackingInstantNow {
    /// Check if time `t` has been reached yet (and remember that we want to wake up then)
    ///
    /// Also available via [`PartialOrd`]
    fn cmp(&self, t: Instant) -> std::cmp::Ordering {
        instant_cmp(&self.earliest, self.now, t)
    }
}
define_PartialOrd_via_cmp! { TrackingInstantNow, Instant, }

impl<'i> TrackingInstantOffsetNow<'i> {
    /// Check if the offset current time has advanced to `t` yet (and remember for wakeup)
    ///
    /// Also available via [`PartialOrd`]
    ///
    /// ### Alternative description
    ///
    /// Checks if the current time has advanced to `offset` *after* `t`,
    /// where `offset` was passed to `TrackingInstantNow::checked_sub`.
    fn cmp(&self, t: Instant) -> std::cmp::Ordering {
        instant_cmp(self.earliest, self.threshold, t)
    }
}
define_PartialOrd_via_cmp! { TrackingInstantOffsetNow<'_>, Instant, }

// Combined TrackingNow cmp and PartialOrd impls done via derive-adhoc

//----- checked_sub (constructor for Instant offset tracker) -----

impl TrackingInstantNow {
    /// Return a tracker representing a specific offset before the current time
    ///
    /// You can use this to pre-calculate an offset from the current time,
    /// to compare other `Instant`s with.
    ///
    /// This can be convenient to avoid repetition;
    /// also,
    /// when working with checked time arithmetic,
    /// this can helpfully centralise the out-of-bounds error handling site.
    pub fn checked_sub(&self, offset: Duration) -> Option<TrackingInstantOffsetNow> {
        let threshold = self.now.checked_sub(offset)?;
        Some(TrackingInstantOffsetNow {
            threshold,
            earliest: &self.earliest,
        })
    }
}

impl TrackingNow {
    /// Return a tracker representing an `Instant` a specific offset before the current time
    ///
    /// See [`TrackingInstantNow::checked_sub()`] for more details.
    ///
    /// ### `Instant`-only
    ///
    /// The returned tracker handles only `Instant`s,
    /// for reasons relating to clock warps:
    /// broadly, waiting for a particular `SystemTime` must always be done
    /// by working with the future `SystemTime` at which to wake up;
    /// whereas, waiting for a particular `Instant` can be done by calculating `Durations`s.
    ///
    /// For the same reason there is no
    /// `.checked_sub()` method on [`TrackingSystemTimeNow`].
    pub fn checked_sub(&self, offset: Duration) -> Option<TrackingInstantOffsetNow> {
        self.instant.checked_sub(offset)
    }
}
