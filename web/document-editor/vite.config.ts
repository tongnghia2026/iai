import { defineConfig } from 'vite'

export default defineConfig({
  base: './',
  build: {
    cssCodeSplit: false,
    emptyOutDir: true,
    rollupOptions: {
      output: {
        entryFileNames: 'editor.js',
        assetFileNames: assetInfo =>
          assetInfo.names?.some(name => name.endsWith('.css'))
            ? 'editor.css'
            : 'assets/[name][extname]'
      }
    }
  }
})
