// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `Members can find me` for group info and channel info
 * (`docs/ux/who-can-find-me.md` section 5), taking the id in the form each
 * info screen already holds: a group's `groupIdB64` (standard base64), a
 * channel's 32-character hex id. Renders nothing for an id it cannot read,
 * and nothing while the setting does not include groups and channels.
 */
import { Show, type JSX } from "solid-js";

import { base64Decode } from "../auth/account-id";
import { MembersCanFindMeRow } from "./WhoCanFindMe";
import { channelIdBytes } from "./who-can-find-me";

export function MembersCanFindMeSwitch(
  props: { readonly kind: "group"; readonly groupIdB64: string } | { readonly kind: "channel"; readonly idHex: string },
): JSX.Element {
  const id = (): Uint8Array | null => {
    if (props.kind === "channel") return channelIdBytes(props.idHex);
    try {
      return base64Decode(props.groupIdB64);
    } catch {
      return null;
    }
  };
  return (
    <Show when={id()} keyed>
      {(bytes) => <MembersCanFindMeRow kind={props.kind} id={bytes} />}
    </Show>
  );
}
