# Farbstrom — thin wrappers around the two compose files.
#
#   make deploy   start on a deploy host (pulls the published image)
#   make update   pull the newest image and recreate (live sessions survive)
#   make dev      build frontend + image from source, bind-mount ./www for live edits
#   make logs     follow all services' logs
#   make status   per-service supervisord state inside the container
#   make down     stop and remove the container
#
# `dev` selects both files; everything else uses the base only (image deploy).

BASE := -f docker-compose.yml
DEV  := -f docker-compose.yml -f docker-compose.dev.yml

.PHONY: deploy update dev frontend-build logs status down

deploy:
	docker compose $(BASE) up -d

update: frontend-build
	docker compose $(BASE) pull
	docker compose $(BASE) up -d

# The dev overlay bind-mounts ./www over the image's baked /www, so www/dist
# must exist on the host first — build it (tsc) before bringing the stack up,
# else /admin serves a blank page (missing /dist/admin/main.js).
frontend-build:
	cd frontend && npm ci && npm run build

dev: frontend-build
	docker compose $(DEV) up -d --build

logs:
	docker compose $(BASE) logs -f

status:
	docker exec farbstrom supervisorctl status

down:
	docker compose $(BASE) down
