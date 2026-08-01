<template>
  <figure class="cast">
    <div class="cast__header">
      <div class="cast__traffic" aria-hidden="true">
        <span class="cast__dot cast__dot--close"></span>
        <span class="cast__dot cast__dot--min"></span>
        <span class="cast__dot cast__dot--max"></span>
      </div>
      <div class="cast__title">{{ computedTitle }}</div>
    </div>
    <div ref="containerRef" class="cast__player"></div>
  </figure>
</template>

<script setup lang="ts">
// The real player (asciinema-player, real terminal emulator, real cursor
// positioning) rather than the hand-rolled "strip ANSI, concatenate output"
// implementation this replaces (#77). That approach is correct only for a
// shell recording, where output arrives as lines with real newlines in the
// stream — it cannot render a full-screen TUI, which repaints in place via
// cursor-positioning escape sequences and never emits one. Measured on
// assets/demo.cast: 28213 bytes of output, zero newline characters, before
// or after stripping ANSI — the line structure was never in the stream to
// recover, it was in the escape sequences the old component discarded.
//
// This component is shared with SpechtLabs/kush (byte-identical file,
// confirmed by diff before writing this). kush's own casts are shell
// sessions, which is why the old implementation's limitation never showed
// there — verified the same way, by counting newlines in one of its .cast
// files (32 across 473 output bytes, plenty of real line structure). One
// mechanism for both rather than two: the real player renders a shell
// recording correctly too, so there is no shell-only case left that would
// justify keeping the old code path around.
//
// asciicast v3 (this project's format, relative inter-event deltas, not
// v2's absolute timestamps) needs asciinema-player >=3.10.0 — confirmed
// against the project's own compatibility notes rather than assumed;
// docs/package.json pins 3.17.0.
import { onBeforeUnmount, onMounted, ref, computed } from 'vue'
import { create, type Player } from 'asciinema-player'
import 'asciinema-player/dist/bundle/asciinema-player.css'

const props = withDefaults(defineProps<{
  src: string
  title?: string
  rows?: number
  autoplay?: boolean
  loop?: boolean
  speed?: number
}>(), {
  rows: 16,
  autoplay: true,
  loop: true,
  speed: 0.75,
})

const computedTitle = computed(() => props.title?.trim() || 'Terminal recording')
const containerRef = ref<HTMLElement | null>(null)
let player: Player | null = null

onMounted(() => {
  if (!containerRef.value) return
  // `theme: 'auto'` (player >=3.8) tracks the site's light/dark toggle
  // instead of a fixed palette. `fit: 'width'` matches the old
  // implementation's behaviour of filling the figure's width regardless of
  // the recording's own column count.
  player = create(props.src, containerRef.value, {
    rows: props.rows,
    autoPlay: props.autoplay,
    loop: props.loop,
    speed: props.speed,
    theme: 'auto',
    fit: 'width',
    terminalFontSize: 'small',
  })
})

onBeforeUnmount(() => {
  player?.dispose()
  player = null
})
</script>

<style scoped>
.cast {
  max-width: 960px;
  margin: 1.25rem auto;
  overflow: hidden;
  border: 1px solid var(--vp-c-divider);
  border-radius: 8px;
  background: #111418;
  box-shadow: 0 8px 22px rgb(0 0 0 / 10%);
  text-align: left;
}

.cast__header {
  display: grid;
  grid-template-columns: auto 1fr;
  align-items: center;
  gap: 0.75rem;
  min-height: 2.35rem;
  padding: 0 0.75rem;
  border-bottom: 1px solid rgb(255 255 255 / 8%);
  background: #1b2027;
}

.cast__traffic {
  display: flex;
  gap: 0.42rem;
}

.cast__dot {
  width: 0.72rem;
  height: 0.72rem;
  border-radius: 50%;
}

.cast__dot--close {
  background: #ff5f57;
}

.cast__dot--min {
  background: #ffbd2e;
}

.cast__dot--max {
  background: #28c840;
}

.cast__title {
  overflow: hidden;
  color: #d8dee9;
  font-size: 0.82rem;
  font-weight: 600;
  line-height: 1.2;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.cast__player {
  width: 100%;
}

/* asciinema-player renders its own controls, cursor and colour scheme; this
   component only owns the chrome above it. */
.cast__player :deep(.ap-player) {
  border-radius: 0;
}
</style>
