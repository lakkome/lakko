// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { render } from "solid-js/web";

import "../../web/src/styles.css";
import "./panel.css";

import { App } from "./App";

const root = document.getElementById("root");
if (root !== null) render(() => <App />, root);
