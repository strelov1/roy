import { mount } from 'svelte';
import App from './App.svelte';
import './app.css';
import { initTheme } from './lib/theme.svelte';

// shadcn-svelte's dark variant keys off a `.dark` class on the root.
// `initTheme` reads the user's saved preference (localStorage) or falls
// back to the OS pref, applies it once, and keeps tracking the media
// query for the `system` mode.
initTheme();

const target = document.getElementById('app');
if (!target) throw new Error('#app element not found in index.html');

const app = mount(App, { target });

export default app;
