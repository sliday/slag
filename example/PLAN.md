;; CRUCIBLE Tue Jan 27 10:13:45 CET 2026
;; Blueprint: BLUEPRINT.md
(ingot :id "i1" :status ore :solo t :grade 1 :heat 0 :max 5 :proof "test -f slag/wrangler.toml" :work "Verify wrangler config exists")
(ingot :id "i2" :status ore :solo t :grade 1 :heat 0 :max 5 :proof "test -f slag/index.html" :work "Verify HTML entry point exists")
(ingot :id "i3" :status ore :solo t :grade 1 :heat 0 :max 5 :proof "grep -q 'slag-dev' slag/wrangler.toml" :work "Verify project name in wrangler config")
(ingot :id "i4" :status ore :solo nil :grade 2 :heat 0 :max 5 :proof "cd slag && npx wrangler pages project list | grep -q slag-dev" :work "Verify Cloudflare Pages project exists")
(ingot :id "i5" :status ore :solo nil :grade 2 :heat 0 :max 5 :proof "cd slag && npm run build && test -d dist" :work "Verify build produces dist directory")
(ingot :id "i6" :status ore :solo nil :grade 3 :heat 0 :max 8 :proof "curl -s https://slag.dev | grep -q 'slag orchestrator'" :work "Deploy to Cloudflare Pages and verify live")
(ingot :id "i7" :status ore :solo t :grade 1 :heat 0 :max 5 :proof "curl -s https://slag.dev | grep -q 'viewport'" :work "Verify mobile-first viewport meta")
(ingot :id "i8" :status ore :solo nil :grade 2 :heat 0 :max 5 :proof "cd slag && npx wrangler pages deployment list --project-name=slag-dev | head -n 2 | grep -q 'Success'" :work "Verify successful deployment status")
