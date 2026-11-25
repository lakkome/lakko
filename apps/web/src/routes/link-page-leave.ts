// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Where a link page (`/i/`, `/c/`) goes when the person closes it.
 *
 * A Lakko link clicked in a message opens its screen in place
 * (`docs/ux/message-links.md` section 2), so closing it returns to the screen
 * the person came from. A page the tab was opened with (a link from outside)
 * has nothing to return to inside the app and goes to Chats.
 */
import { useLocation, useNavigate } from "@solidjs/router";

import { linkPageExit, type LinkPageExit } from "../platform/lakko-link";
import { entryInternalPath } from "./origin-history";
import { paths } from "./paths";

/** The close action of a link page, for the current location. */
export function useLeaveLinkPage(): { readonly exit: () => LinkPageExit; readonly leave: () => void } {
  const location = useLocation();
  const navigate = useNavigate();
  const exit = (): LinkPageExit => linkPageExit(location.pathname, location.hash, entryInternalPath());
  return {
    exit,
    leave: () => {
      if (exit() === "back") navigate(-1);
      else navigate(paths.home, { replace: true });
    },
  };
}
