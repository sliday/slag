import {Config} from '@remotion/cli/config';

// Frames go out as PNG so render.sh can encode both formats from one
// lossless source.
Config.setVideoImageFormat('png');
Config.setOverwriteOutput(true);
