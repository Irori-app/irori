<script setup lang="ts">
/** The mark and the stroke-drawn wordmark; the letters take the text colour. */
defineProps<{ id: string }>()
</script>

<template>
  <span class="inline-flex items-center gap-2.5">
    <img src="/brand/irori-mark.svg" alt="" width="36" height="36" class="size-9 shrink-0" />
    <svg viewBox="-24 96 1148 604" class="h-6 w-auto" fill="none" role="img" aria-label="Irori">
      <defs>
        <linearGradient :id="`${id}-spark`" x1="0" y1="112" x2="0" y2="238" gradientUnits="userSpaceOnUse">
          <stop offset="0" stop-color="#ff5f2a" />
          <stop offset="1" stop-color="#ffb24f" />
        </linearGradient>
      </defs>
      <g stroke="currentColor" stroke-width="72" stroke-linecap="round">
        <path d="M36 316V604" />
        <path d="M164 604V316M164 470C164 370 204 316 304 316" />
        <circle cx="540" cy="460" r="144" />
        <path d="M812 604V316M812 470C812 370 852 316 952 316" />
        <path d="M1064 316V604" />
      </g>
      <g :fill="`url(#${id}-spark)`">
        <path d="M44 112C56 150 80 168 80 194A44 44 0 0 1 -8 194C-8 164 26 150 44 112Z" />
        <path d="M1072 112C1084 150 1108 168 1108 194A44 44 0 0 1 1020 194C1020 164 1054 150 1072 112Z" />
      </g>
    </svg>
  </span>
</template>
