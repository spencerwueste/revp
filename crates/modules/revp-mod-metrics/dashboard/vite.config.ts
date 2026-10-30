import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { viteSingleFile } from "vite-plugin-singlefile";
import path from "node:path";

// Built into ONE self-contained dist/index.html, which revp-mod-metrics embeds
// with include_bytes! and serves at /dashboard on the admin listener. So revp
// builds without Node; rerun `npm run build` after changing anything here.
//
// `npm run dev` serves on :5174 and proxies the API to a running revp's admin
// listener (REVP_ADMIN, default http://127.0.0.1:9090).
export default defineConfig({
  plugins: [react(), tailwindcss(), viteSingleFile()],
  base: "./",
  resolve: { alias: { "@": path.resolve(__dirname, "./src") } },
  server: {
    port: 5174,
    proxy: { "/dashboard/api": process.env.REVP_ADMIN ?? "http://127.0.0.1:9090" },
  },
});
