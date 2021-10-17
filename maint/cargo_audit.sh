#!/bin/bash
#
# Run "cargo audit" with an appropriate set of flags.

# List of vulnerabilities to ignore.  It's risky to do this, so we should
# only do this when two circumstances hold:
#   1. The vulnerability doesn't affect us.
#   2. We can't update to an unaffected version.
#   3. We have a plan to not have this vulnerability ignored forever.
#
# If you add anything to this section, make sure to add a comment
# explaining why it's safe to do so.
IGNORE=(
    # This is a vulnerability in the `time` crate.  We don't import
    # `time` directly, but inherit it through the `oldtime` feature
    # in `chrono`.  The vulnerability occurs when somebody messes
    # with the environment while at the same time calling a function
    # that uses localtime_r.
    #
    # Why this doesn't affect us:
    #   * We never use the time crate, and we never mess with local times via the time crate.  We only get the time crate accidentally
    #     because rusqlite builds chrono with its default-features
    #     enabled.
    #
    # Why we can't update to a better version of `time`:
    #   * Chrono's `oldtime` feature requires `time` 0.1.43, and can't
    #     be update to `time` 0.2.x.
    #   * Rusqlite's feature that enables `chrono` support does so by
    #     depending on `chrono` with default features, which includes
    #     `oldtime`.
    #
    # What we can do:
    #  * Get rusqlite to update its dependency on `chrono` to not
    #    include `oldtime`.
    #    (PR: https://github.com/rusqlite/rusqlite/pull/1031 )
    #  * Stop using the `chrono` feature on rusqlite, and do our date
    #    conversions in `tor-dirmgr` manually.
    --ignore RUSTSEC-2020-0071
)

cargo audit -D warnings "${IGNORE[@]}"
