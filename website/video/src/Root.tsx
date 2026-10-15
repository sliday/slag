import '@fontsource/cascadia-code/400.css';
import '@fontsource/cascadia-code/700.css';
import React from 'react';
import {Composition, continueRender, delayRender} from 'remotion';
import {Hero} from './Hero';
import {heroScript} from './script';
import {layout} from './theme';

// Hold every frame until both weights and the block-element subset load.
const fonts = delayRender('Cascadia Code');
Promise.all([
  document.fonts.load(`400 ${layout.termSize}px "Cascadia Code"`, 'slag ░▒▓█'),
  document.fonts.load(`700 ${layout.captionSize}px "Cascadia Code"`, 'SLAG ░▒▓█'),
]).then(() => continueRender(fonts));

export const RemotionRoot: React.FC = () => (
  <Composition
    id="Hero"
    component={Hero}
    durationInFrames={heroScript.duration * layout.fps}
    fps={layout.fps}
    width={layout.width}
    height={layout.height}
    defaultProps={heroScript}
  />
);
