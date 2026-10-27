import { defineConfig } from 'vite';
export default defineConfig({ clearScreen: false, base: './', server: { port: 1420, strictPort: true }, build: { target: 'es2022', outDir: 'dist', emptyOutDir: true } });
