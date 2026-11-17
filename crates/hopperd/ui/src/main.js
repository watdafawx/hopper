import { mount } from 'svelte';
import './app.css';
// Paints the kept theme before the first screen.
import './lib/theme.svelte.js';
import App from './App.svelte';
import { numberFieldsWheel } from './lib/stepper.js';

// The wheel turns a plain number field by its own step.
numberFieldsWheel();

mount(App, { target: document.getElementById('app') });
