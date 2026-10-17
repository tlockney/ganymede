<script setup lang="ts">
import { computed } from "vue";
import { useData } from "vitepress";
import { levelOf, levels } from "../levels.ts";

const { page } = useData();
const level = computed(() => levelOf(page.value.relativePath));
</script>

<template>
  <p v-if="level" class="level" :class="`level-${level}`">
    <span class="level-label">{{ levels[level].label }}</span>
    <span class="level-note">{{ levels[level].note }}</span>
  </p>
</template>
