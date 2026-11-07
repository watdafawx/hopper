import { mount } from 'svelte';
import './app.css';
// Paints the kept theme before the first screen.
import './lib/theme.svelte.js';
import App from './App.svelte';

mount(App, { target: document.getElementById('app') });
