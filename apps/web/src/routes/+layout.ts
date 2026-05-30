import { redirect } from '@sveltejs/kit';
import { readBearer } from '$lib/bearer';
import type { LayoutLoad } from './$types';

// SPA only — disable SSR (adapter-static prerenders index.html).
export const ssr = false;
export const prerender = true;
export const trailingSlash = 'always';

// Bearer guard: every route except `/login` requires *some* token in
// localStorage. The "Skip · dev mode" button on /login writes the
// `dev` sentinel so a developer can browse the SPA without a real JWT;
// lib/api/* treats that value as "do not send Authorization header" once
// the API client lands (hq-fe-build.2).
export const load: LayoutLoad = ({ url }) => {
  if (url.pathname.startsWith('/login')) return {};
  const bearer = readBearer();
  if (!bearer) {
    throw redirect(307, '/login');
  }
  return { bearer };
};
