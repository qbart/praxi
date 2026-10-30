# Praxi

praxi (from praxinoscope animation device) is the video codec for games in pure rust. It uses very little CPU/GPU power, handles multi-channel audio beautifully, and decodes lightning-fast without lagging the game engine. It allows for seamless transitions between gameplay and video, supports transparency channels (alpha transparency), and handles multi-stream playback.

The player can also emit events (e.g. playback started/finished, loop points, custom timed markers embedded in the video) so the game engine can hook into them.
