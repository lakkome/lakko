// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * Where a call-screen menu that opens from a control-bar button sits
 * horizontally: the End menu of the person who started a group call
 * (`docs/ux/calls.md` section 4.1.2) and the Participants / invite menu
 * (`docs/ux/call-links.md` section 3).
 *
 * The rule is the web client's `anchorToControl`: directly above its own
 * button, centred on the button's centre, then slid inward so it keeps
 * [CALL_MENU_EDGE_GAP_DP] from the call screen's left and right edges. Before
 * this the End menu was glued to the screen's right edge and the Participants
 * menu centred on the screen, neither attached to the button that opened it.
 *
 * Pure and Android-free, so the placement is unit-tested on the host JVM.
 */

/** The gap a call-screen menu keeps from the call screen's side edges. */
public const val CALL_MENU_EDGE_GAP_DP: Float = 12f

/**
 * The menu's left edge, in the call screen's own coordinates.
 *
 * All values share one unit (pixels in the client). A menu wider than the
 * screen less both gaps is placed at [edgeGap]: its left edge stays on the
 * screen, which is where its rows start.
 *
 * @param buttonCenterX the horizontal centre of the button that opened it.
 * @param menuWidth the menu's own measured width.
 * @param areaWidth the call screen's width.
 * @param edgeGap the gap kept from either side edge.
 */
public fun anchoredMenuLeft(
    buttonCenterX: Float,
    menuWidth: Float,
    areaWidth: Float,
    edgeGap: Float,
): Float {
    val centred = buttonCenterX - menuWidth / 2f
    val rightmost = maxOf(edgeGap, areaWidth - menuWidth - edgeGap)
    return minOf(maxOf(centred, edgeGap), rightmost)
}
