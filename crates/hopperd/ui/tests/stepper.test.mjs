import test from 'node:test';
import assert from 'node:assert/strict';
import { sliderStep, stepFor, stepped, fromSlider, stepText, wheelNotches } from '../src/lib/stepper.js';

// These are the cases of hopper-core's configedit/stepper.rs: the web UI and
// the desktop app must agree on every one.

test('slider steps are tidy numbers', () => {
  assert.equal(sliderStep(1), 0.01);
  assert.equal(sliderStep(2), 0.01);
  assert.equal(sliderStep(3), 0.01);
  assert.equal(sliderStep(16), 0.1);
  assert.equal(sliderStep(100), 1);
  assert.equal(sliderStep(255), 1);
  assert.equal(sliderStep(500), 5);
  assert.equal(sliderStep(5000), 50);
  assert.equal(sliderStep(0), 1);
  assert.equal(sliderStep(NaN), 1);
  for (const span of [0.5, 1, 7, 16, 99, 250, 3000, 100000]) assert.ok(span / sliderStep(span) >= 99.9, String(span));
});

test('the step for a value', () => {
  assert.equal(stepFor(true, 5, null), 1);
  assert.equal(stepFor(false, 0.35, [0, 1]), 0.01);
  assert.equal(stepFor(false, 2.5, [0, 16]), 0.1);
  assert.equal(stepFor(false, 250, null), 1);
  assert.equal(stepFor(false, 2.5, null), 0.1);
  assert.equal(stepFor(false, 0, [0, null]), 0.1);
  assert.equal(stepFor(false, 0.25, null), 0.01);
  // A huge span is no slider: by size.
  assert.equal(stepFor(false, 5, [0, 1e9]), 0.1);
});

test('a whole number steps and stops at the ends', () => {
  assert.equal(stepped(true, 196, 1, 1, [0, 255]), 197);
  assert.equal(stepped(true, 196, -2, 1, [0, 255]), 194);
  assert.equal(stepped(true, 255, 1, 1, [0, 255]), 255);
  assert.equal(stepped(true, 0, -1, 1, [0, 255]), 0);
  assert.equal(stepped(true, 250, 10, 1, [0, 255]), 255);
  assert.equal(stepped(true, 3, -10, 1, [0, 255]), 0);
  assert.equal(stepped(true, -5, -1, 1, null), -6);
  assert.equal(stepped(true, 5, 1, 1, [1, null]), 6);
});

test('a float steps without noise', () => {
  let v = 0;
  for (let i = 0; i < 3; i++) v = stepped(false, v, 1, 0.1, [0, 1]);
  assert.equal(v, 0.3);
  assert.equal(stepped(false, 0.35, 1, 0.01, [0, 1]), 0.36);
  assert.equal(stepped(false, 0.99, 5, 0.01, [0, 1]), 1);
  assert.equal(stepped(false, 0.02, -5, 0.01, [0, 1]), 0);
  assert.equal(stepped(false, 0.125, 1, 0.1, null), 0.225);
  assert.equal(stepped(false, 1234.5, 1, 50, [0, 5000]), 1284.5);
});

test('the slider snaps and clamps', () => {
  assert.equal(fromSlider(116.4, 0, 255, 1, true), 116);
  assert.equal(fromSlider(300, 0, 255, 1, true), 255);
  assert.equal(fromSlider(-3, 0, 255, 1, true), 0);
  assert.equal(fromSlider(0.3500002, 0, 1, 0.01, false), 0.35);
  assert.equal(fromSlider(0.999, 0, 1, 0.01, false), 1);
  assert.equal(fromSlider(2.56, 0, 16, 0.1, false), 2.6);
  assert.equal(fromSlider(-0.47, -1, 1, 0.05, false), -0.45);
  assert.equal(fromSlider(1234, 0, 5000, 50, false), 1250);
  assert.equal(fromSlider(16, 0, 16, 0.3, false), 16);
  assert.equal(fromSlider(0.6, 0, 1, 1, true), 1);
});

test('a typed number steps as text', () => {
  assert.equal(stepText('15', 1, 1, 1, 1440, true), '16');
  assert.equal(stepText(' 15 ', -1, 1, 1, 1440, true), '14');
  assert.equal(stepText('1', -1, 1, 1, 1440, true), '1');
  assert.equal(stepText('65535', 1, 1, 1, 65535, true), '65535');
  assert.equal(stepText('8799', 10, 1, 1, 65535, true), '8809');
  assert.equal(stepText('', 1, 1, 0, null, true), '1');
  assert.equal(stepText('abc', -1, 1, 0, null, true), '0');
  assert.equal(stepText('', 1, 1, null, null, true), '1');
  assert.equal(stepText('1.5', 1, 0.5, 0, 100000, false), '2');
  assert.equal(stepText('0', -1, 0.5, 0, null, false), '0');
  assert.equal(stepText('0.1', 2, 0.1, 0, null, false), '0.3');
});

test('a mouse notch is one step, up is positive; a touchpad adds up', () => {
  assert.equal(wheelNotches({ deltaY: -100, deltaMode: 0 }), 1);
  assert.equal(wheelNotches({ deltaY: 100, deltaMode: 0 }), -1);
  assert.equal(wheelNotches({ deltaY: -3, deltaMode: 1 }), 1);
  // Shift + wheel arrives sideways.
  assert.equal(wheelNotches({ deltaY: 0, deltaX: -100, deltaMode: 0, shiftKey: true }), 1);
  // Small pixel moves: one step per 40.
  assert.equal(wheelNotches({ deltaY: -30, deltaMode: 0 }), 0);
  assert.equal(wheelNotches({ deltaY: -30, deltaMode: 0 }), 1);
  assert.equal(wheelNotches({ deltaY: 0, deltaMode: 0 }), 0);
});
