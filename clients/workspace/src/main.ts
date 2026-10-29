import { mount } from "svelte";
import App from "./App.svelte";
import "./styles.css";

const target = document.getElementById("root");
if (!target) throw new Error("workspace_root_missing");

mount(App, { target });
