import path from "path"
import { cpSync, mkdirSync } from "fs"
import react from "@vitejs/plugin-react"
import { defineConfig, type Plugin } from "vite"
import { inspectAttr } from 'plugin-inspect-react-code'

function copyBrandAssets(): Plugin {
  const source = path.resolve(__dirname, "../assets/brand")
  const destination = path.resolve(__dirname, "dist/assets/brand")

  return {
    name: "copy-brand-assets",
    closeBundle() {
      mkdirSync(destination, { recursive: true })
      cpSync(source, destination, { recursive: true })
    },
  }
}

// https://vite.dev/config/
export default defineConfig({
  base: './',
  plugins: [inspectAttr(), react(), copyBrandAssets()],
  server: {
    port: 3000,
  },
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
});
