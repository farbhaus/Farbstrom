// Privacy page: theme it (colors, favicon) and fill the wordmark with the brand
// name. Skips the background image — this is a text-heavy legal page.

import { applyBranding } from '../shared/branding.js';

void applyBranding({ setTitle: false, skipBg: true });
