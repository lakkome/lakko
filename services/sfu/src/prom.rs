// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Prometheus metrics exporter for the SFU.
//!
//! The `metrics` crate is a recorder-agnostic facade; we wire
//! the Prometheus recorder behind it and let any module
//! sprinkle `metrics::counter!` / `metrics::gauge!` calls
//! against the canonical names listed in [`names`] below.
//!
//! The recorder and the `/metrics` listener are `lakko-metrics`'s
//! (ADR-0096), shared with every service; this module names the SFU's own
//! metrics and describes them. The listener binds on a separate port
//! (default 9641) so nginx applies mTLS exclusively to the metrics endpoint
//! without affecting the WebSocket signaling path.
//!
//! Wire-shape stability is a contract — every alert rule in
//! `infra/prometheus/rules/calls.yml` references one of the
//! names below. Renaming a metric is a wire-format break.

use tracing::info;

/// Canonical metric names. The Prometheus alert rules in
/// `infra/prometheus/rules/calls.yml` quote these — keep them
/// in lock-step on any rename.
///
/// All names follow the `lakko_sfu_*` namespace so a
/// federation scrape can dedupe with other Lakko services
/// without prefix-clash.
pub mod names {
    /// Total number of peers currently registered.
    pub const PEERS_ACTIVE: &str = "lakko_sfu_peers_active";
    /// Cumulative count of accepted WS upgrades.
    pub const PEERS_ADMITTED_TOTAL: &str = "lakko_sfu_peers_admitted_total";
    /// Cumulative count of rejected WS upgrades — JWT
    /// verify failed, kid unknown, expired, etc.
    pub const PEERS_REJECTED_TOTAL: &str = "lakko_sfu_peers_rejected_total";
    /// Cumulative count of distinct calls observed since
    /// boot: a room's first admitted socket, counted by
    /// `crate::call_lifetime::CallLifetimes::admit`. A call revived
    /// within its empty grace is not counted again.
    pub const CALLS_TOTAL: &str = "lakko_sfu_calls_total";
    /// Rooms with at least one connected socket (ADR-0096 section 3). A
    /// count, never a series per room.
    pub const ROOMS_ACTIVE: &str = "lakko_sfu_rooms_active";
    /// Cumulative count of peers the signaling socket dropped
    /// because they sent nothing for the idle window (see
    /// `SignalingLiveness`). A live peer answers protocol pings,
    /// so every increment is a peer whose network went away
    /// without a close frame; a steady rate on a healthy network
    /// means a client stack has stopped answering pings.
    pub const PEERS_DROPPED_IDLE_TOTAL: &str = "lakko_sfu_peers_dropped_idle_total";
    /// Cumulative count of signaling frames received from
    /// peers. A single un-labelled bucket: the typed signaling
    /// protocol exists (`lakko-sfu-proto`), but this counter is
    /// not yet broken out by frame `kind` — a per-kind label is
    /// an observability nice-to-have, not a correctness gap.
    pub const SIGNALING_FRAMES_TOTAL: &str = "lakko_sfu_signaling_frames_total";
    /// JWKS cache age, seconds since the last successful
    /// refresh. The `LakkoSfuJwksStale` alert (future) will
    /// fire when this exceeds the configured refresh cadence
    /// by a wide margin.
    pub const JWKS_CACHE_AGE_SECS: &str = "lakko_sfu_jwks_cache_age_seconds";
    /// Cumulative count of RTP packets successfully forwarded
    /// to a downstream peer (one increment per destination
    /// peer per packet — a publisher packet fanned out to N
    /// subscribers bumps this by N). Lets a dashboard see
    /// aggregate fanout volume; a flat line while
    /// `peers_active > 1` means media isn't flowing.
    ///
    /// The name says "frames" because the SFU forwarded whole
    /// frames until ADR-0066; it is kept so dashboards and
    /// alert rules do not break. The unit is a packet.
    pub const MEDIA_FRAMES_FORWARDED_TOTAL: &str = "lakko_sfu_media_frames_forwarded_total";
    /// Cumulative count of fanout attempts that did NOT reach
    /// the wire, one per packet per destination, labelled
    /// `reason` (`no_writer` — renegotiation not yet open,
    /// transport not connected, or peer torn down;
    /// `no_codec_match` — destination codec set excludes the
    /// packet's codec; `write_failed` — str0m refused the
    /// write; `wrong_layer` — a simulcast packet for a layer
    /// this subscriber is not receiving, dropped by design;
    /// `stale` — a packet older than a simulcast switch point,
    /// whose downstream sequence number is already taken;
    /// `not_wanted` — a packet of a main or middle track the
    /// subscriber does not show, ADR-0070 step 5, dropped by
    /// design). A
    /// spike in `no_writer` is the signal that subscribers are
    /// starved (renegotiation stuck), which is the
    /// fanout-fairness alarm; `wrong_layer` is expected traffic
    /// for a simulcast publisher, not an alarm.
    pub const MEDIA_FRAMES_DROPPED_TOTAL: &str = "lakko_sfu_media_frames_dropped_total";
    /// Cumulative count of simulcast layer switches the bandwidth policy made,
    /// labelled `direction` (`up` — recovery / `down` — congestion). A high
    /// `down` rate signals subscribers are frequently bandwidth-constrained; a
    /// churny up/down ratio near 1 hints the hysteresis window is too tight.
    pub const SIMULCAST_LAYER_SWITCHES_TOTAL: &str = "lakko_sfu_simulcast_layer_switches_total";
    /// Cumulative count of video FEC packets (ULPFEC inside RED) received
    /// from publishers. Only moves with `LAKKO_SFU_VIDEO_FEC` on and a
    /// publisher that sends FEC. See `crate::fec_receive`.
    pub const VIDEO_FEC_PACKETS_TOTAL: &str = "lakko_sfu_video_fec_packets_total";
    /// Cumulative count of video media packets that were lost on a
    /// publisher's leg and that the SFU rebuilt from the publisher's FEC and
    /// forwarded. Against `VIDEO_FEC_PACKETS_TOTAL` it says what the FEC
    /// bought.
    pub const VIDEO_FEC_RECOVERED_TOTAL: &str = "lakko_sfu_video_fec_recovered_total";
    /// Cumulative count of video FEC packets the SFU computed for a
    /// subscriber's leg and sent, one per subscriber per publisher FEC
    /// packet whose protected packets all went to that subscriber. See
    /// `crate::fec_send`.
    pub const VIDEO_FEC_SENT_TOTAL: &str = "lakko_sfu_video_fec_sent_total";
    /// Cumulative count of REMB requests the SFU made toward publishers
    /// under `LAKKO_SFU_BITRATE_CONTROL=server` (ADR-0068), one per
    /// publisher per REMB round. Flat with publishers sending video means
    /// no publisher is being told a bitrate. See `crate::uplink_estimate`.
    pub const BITRATE_REMB_SENT_TOTAL: &str = "lakko_sfu_bitrate_remb_sent_total";
    /// Cumulative count of `publish-budget` frames sent to publishers
    /// (ADR-0068, amendment of 2026-10-11), labelled `why` (`first`,
    /// `lower`, `senders`, `higher`; see `crate::publish_budget::Why`).
    /// Only under `LAKKO_SFU_BITRATE_CONTROL=server` with
    /// `LAKKO_SFU_PUBLISH_BUDGET` on. A `lower` count that climbs steadily
    /// for one call is a publisher whose uplink keeps being cut.
    pub const PUBLISH_BUDGET_SENT_TOTAL: &str = "lakko_sfu_publish_budget_sent_total";
    /// Cumulative count of uplink fallback state changes, one per publisher
    /// video track per change, labelled `direction` (`fallback`: the
    /// publisher's uplink is in trouble and its subscribers were moved to
    /// its light simulcast layer; `full`: they were moved back). Only a
    /// publisher that sends simulcast can move it. A publisher that falls
    /// back every few tens of seconds has an uplink that drops in bursts.
    /// See `crate::uplink_fallback`.
    pub const UPLINK_FALLBACK_SWITCHES_TOTAL: &str = "lakko_sfu_uplink_fallback_switches_total";
    /// Cumulative count of RTP packets forwarded on light companion
    /// tracks: the second downstream track a subscriber gets for a
    /// simulcast video track, carrying its light layer all the time
    /// (`crate::media::TrackRole`). Each is also counted in
    /// `MEDIA_FRAMES_FORWARDED_TOTAL`; the difference of the two is what
    /// went out on main tracks. Only moves with a publisher that sends
    /// simulcast.
    pub const LIGHT_COMPANION_PACKETS_FORWARDED_TOTAL: &str =
        "lakko_sfu_media_light_companion_packets_forwarded_total";
    /// Cumulative count of second copies of light companion packets sent
    /// to subscribers (ADR-0070 step 3; `LAKKO_SFU_LIGHT_DUPLICATE`). Each
    /// goes out as a retransmission nobody asked for, some tens of
    /// milliseconds after the packet itself. Against
    /// `LIGHT_COMPANION_PACKETS_FORWARDED_TOTAL` it should be close to one
    /// for one; far below means subscribers negotiated no RTX.
    pub const LIGHT_DUPLICATES_SENT_TOTAL: &str = "lakko_sfu_media_light_duplicates_sent_total";
    /// Cumulative count of times the minimum playout delay of a
    /// (subscriber, publisher) pair was raised after a burst of loss on
    /// the publisher's uplink or the subscriber's downlink (ADR-0070 step
    /// 4; `LAKKO_SFU_PLAYOUT_DELAY`). A pair is raised at most once per
    /// three minutes while bursts keep coming. See `crate::playout_delay`.
    pub const PLAYOUT_DELAY_RAISES_TOTAL: &str = "lakko_sfu_playout_delay_raises_total";
    /// Cumulative count of changes of what a subscriber is given of a
    /// publisher's video under downlink control (ADR-0070 step 6;
    /// `LAKKO_SFU_DOWNLINK_CONTROL`), one per (subscriber, publisher) per
    /// change, labelled `direction`: `down` when a larger track was given
    /// up (the subscriber's downlink is short, or it asked for less), `up`
    /// when one was given back. See `crate::downlink_allocation`.
    pub const DOWNLINK_GIVEN_CHANGES_TOTAL: &str = "lakko_sfu_downlink_given_changes_total";
    /// Cumulative count of probes of subscribers' downlinks that ended
    /// (ADR-0070 step 6), labelled `outcome`: `carried` (the path carried
    /// the rate of the next larger set, which is then given), or why not
    /// (`trouble`, `loss`, `short`, `no-feedback`, `disturbed`). See
    /// `crate::downlink_probe`.
    pub const DOWNLINK_PROBES_TOTAL: &str = "lakko_sfu_downlink_probes_total";
    /// Cumulative bytes of padding sent for those probes: copies of
    /// packets already sent, on retransmission streams.
    pub const DOWNLINK_PROBE_PADDING_BYTES_TOTAL: &str =
        "lakko_sfu_downlink_probe_padding_bytes_total";
}

/// Install the process-wide recorder ([`lakko_metrics::install`], which also
/// records `lakko_build_info` and the RED layer's buckets) and describe the
/// SFU's own metrics. Must be called before anything emits a metric:
/// `metrics::counter!` calls before the recorder exists are lost.
///
/// # Errors
///
/// [`lakko_metrics::MetricsError::Install`] when another recorder is already
/// installed in the process.
pub fn install() -> Result<lakko_metrics::Metrics, lakko_metrics::MetricsError> {
    let metrics = lakko_metrics::install("sfu", crate::VERSION)?;
    info!("metrics: prometheus recorder installed");
    // Touch every counter so the first scrape sees a
    // zero value rather than an empty exposition. This
    // matters for the alert-rule pattern
    // `increase(metric[5m]) == 0` — without an initial
    // sample, the rate evaluator yields NaN and the
    // alert stays NoData.
    metrics::describe_counter!(
        names::PEERS_ADMITTED_TOTAL,
        "Cumulative count of accepted WS upgrades"
    );
    metrics::describe_counter!(
        names::PEERS_REJECTED_TOTAL,
        "Cumulative count of rejected WS upgrades"
    );
    metrics::describe_counter!(
        names::CALLS_TOTAL,
        "Cumulative count of distinct calls observed since boot"
    );
    metrics::describe_counter!(
        names::PEERS_DROPPED_IDLE_TOTAL,
        "Peers dropped by the signaling socket for sending nothing during the idle window"
    );
    metrics::describe_counter!(
        names::SIGNALING_FRAMES_TOTAL,
        "Cumulative count of signaling frames, labelled by `direction` \
         (inbound = received from a peer / outbound = broadcast to peers) \
         and `kind` (the typed frame kind, or `invalid` for an \
         undecodable inbound frame)"
    );
    metrics::describe_gauge!(names::PEERS_ACTIVE, "Number of peers currently registered");
    metrics::describe_gauge!(
        names::ROOMS_ACTIVE,
        "Rooms with at least one connected socket"
    );
    metrics::describe_gauge!(
        names::JWKS_CACHE_AGE_SECS,
        "JWKS cache age in seconds since last successful refresh"
    );
    metrics::describe_counter!(
        names::MEDIA_FRAMES_FORWARDED_TOTAL,
        "RTP frames forwarded to a downstream peer (one per destination per frame)"
    );
    metrics::describe_counter!(
        names::MEDIA_FRAMES_DROPPED_TOTAL,
        "Fanout attempts that did not reach the wire, labelled by reason"
    );
    metrics::describe_counter!(
        names::SIMULCAST_LAYER_SWITCHES_TOTAL,
        "Simulcast layer switches the bandwidth policy made, labelled by \
         `direction` (up = recovery / down = congestion)"
    );
    // The three video FEC counters are described and, like the labelled
    // ones, not pre-initialised: with the feature off they never appear.
    metrics::describe_counter!(
        names::VIDEO_FEC_PACKETS_TOTAL,
        "Video FEC packets received from publishers"
    );
    metrics::describe_counter!(
        names::VIDEO_FEC_RECOVERED_TOTAL,
        "Video media packets rebuilt from a publisher's FEC and forwarded"
    );
    metrics::describe_counter!(
        names::VIDEO_FEC_SENT_TOTAL,
        "Video FEC packets computed for and sent to subscribers"
    );
    // Like the FEC counters, described and not pre-initialised: it only
    // appears under `LAKKO_SFU_BITRATE_CONTROL=server`.
    metrics::describe_counter!(
        names::BITRATE_REMB_SENT_TOTAL,
        "REMB requests toward publishers under server-side bitrate control"
    );
    metrics::describe_counter!(
        names::PUBLISH_BUDGET_SENT_TOTAL,
        "publish-budget frames sent to publishers under server-side bitrate control"
    );
    // Described and not pre-initialised: it only appears once a
    // simulcast publisher has a subscriber.
    metrics::describe_counter!(
        names::LIGHT_COMPANION_PACKETS_FORWARDED_TOTAL,
        "RTP packets forwarded on light companion tracks of simulcast video"
    );
    // Described and not pre-initialised, like the counter above: they
    // appear with the first light companion packet sent twice and the
    // first playout delay raised.
    metrics::describe_counter!(
        names::LIGHT_DUPLICATES_SENT_TOTAL,
        "Second copies of light companion packets sent to subscribers"
    );
    metrics::describe_counter!(
        names::PLAYOUT_DELAY_RAISES_TOTAL,
        "Times a subscriber's minimum playout delay for a publisher was raised after a burst"
    );
    // Described and not pre-initialised: labelled, and only under
    // downlink control.
    metrics::describe_counter!(
        names::DOWNLINK_GIVEN_CHANGES_TOTAL,
        "Changes of what a subscriber is given of a publisher's video under downlink control"
    );
    metrics::describe_counter!(
        names::DOWNLINK_PROBES_TOTAL,
        "Probes of subscribers' downlinks that ended, by outcome"
    );
    metrics::describe_counter!(
        names::DOWNLINK_PROBE_PADDING_BYTES_TOTAL,
        "Bytes of padding sent for probes of subscribers' downlinks"
    );
    // Initialise each counter / gauge so it ships a zero
    // sample at the first scrape.
    metrics::counter!(names::PEERS_ADMITTED_TOTAL).absolute(0);
    metrics::counter!(names::PEERS_REJECTED_TOTAL).absolute(0);
    metrics::counter!(names::CALLS_TOTAL).absolute(0);
    metrics::counter!(names::PEERS_DROPPED_IDLE_TOTAL).absolute(0);
    // SIGNALING_FRAMES_TOTAL is now labelled by `direction` + `kind`; an
    // unlabelled zero-init would ship a meaningless empty-label series
    // alongside the real ones, so — like the labelled
    // MEDIA_FRAMES_DROPPED_TOTAL — it is not pre-initialised and first
    // appears once traffic flows.
    metrics::gauge!(names::PEERS_ACTIVE).set(0.0);
    metrics::gauge!(names::ROOMS_ACTIVE).set(0.0);
    metrics::gauge!(names::JWKS_CACHE_AGE_SECS).set(0.0);
    metrics::counter!(names::MEDIA_FRAMES_FORWARDED_TOTAL).absolute(0);
    Ok(metrics)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique() {
        let all = [
            names::PEERS_ACTIVE,
            names::PEERS_ADMITTED_TOTAL,
            names::PEERS_REJECTED_TOTAL,
            names::CALLS_TOTAL,
            names::SIGNALING_FRAMES_TOTAL,
            names::JWKS_CACHE_AGE_SECS,
        ];
        let mut s: Vec<&str> = all.to_vec();
        s.sort_unstable();
        s.dedup();
        assert_eq!(s.len(), all.len());
    }

    #[test]
    fn names_use_lakko_sfu_prefix() {
        let all = [
            names::PEERS_ACTIVE,
            names::PEERS_ADMITTED_TOTAL,
            names::PEERS_REJECTED_TOTAL,
            names::CALLS_TOTAL,
            names::SIGNALING_FRAMES_TOTAL,
            names::JWKS_CACHE_AGE_SECS,
        ];
        for n in all {
            assert!(
                n.starts_with("lakko_sfu_"),
                "metric {n} must start with lakko_sfu_"
            );
        }
    }
}
