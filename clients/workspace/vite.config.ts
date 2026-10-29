import { readdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { fixtureApi } from "./fixtures/server.ts";

// R writes figure text in the standard PDF fonts without embedding them. PDF.js asks
// for its replacement fonts by exact file name, so they ship unhashed under /assets.
function pdfStandardFonts(): Plugin {
  const source = fileURLToPath(new URL("./node_modules/pdfjs-dist/standard_fonts/", import.meta.url));
  return {
    name: "pdfjs-standard-fonts",
    apply: "build",
    generateBundle() {
      for (const name of readdirSync(source)) {
        if (!/^[A-Za-z0-9_.-]+$/.test(name)) continue;
        this.emitFile({ type: "asset", fileName: `assets/pdfjs-fonts/${name}`, source: readFileSync(source + name) });
      }
    },
    configureServer(server) {
      server.middlewares.use("/assets/pdfjs-fonts/", (req, res, next) => {
        const name = (req.url ?? "").replace(/^\//, "").split("?")[0];
        if (!/^[A-Za-z0-9_.-]+$/.test(name)) return next();
        try {
          res.end(readFileSync(source + name));
        } catch {
          next();
        }
      });
    },
  };
}

export default defineConfig(({ command, mode }) => ({
  plugins: [
    svelte(),
    { ...pdfStandardFonts(), apply: () => true },
    ...(command === "serve" && mode === "fixtures" ? [fixtureApi()] : []),
  ],
  server: { host: "127.0.0.1", port: 32110, strictPort: true },
  preview: { host: "127.0.0.1", port: 32111, strictPort: true },
  build: { sourcemap: false },
}));
