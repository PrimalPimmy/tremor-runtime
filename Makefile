APP=traffic-shaping-tool

help:
	@echo "This docker files wraps the tasks:"
	@echo "  build - builds the container using two stage builds"
	@echo "  lint - lints the docker file"
	@echo "  goss - uses the goss rules in docker/goss.yaml to test the image (will also run build)"
	@echo "  all - meta task to execute lint, build and goss"

all: lint build goss

build:
	docker-compose build $(APP)

lint:
	docker run -it --rm --privileged -v $$PWD:/root/ \
		artifactory.service.bo1.csnzoo.com/external/projectatomic/dockerfile-lint \
		dockerfile_lint -p -f docker/$(APP).dockerfile -r default_rules.yaml

goss: build
	GOSS_FILES_PATH=docker/ dgoss run --name "$(APP)-dgoss-test" --rm "wayfair/data-engineering/$(APP)"

demo-containers:
	docker build -f demo/loadgen.dockerfile . -t loadgen
	docker build -f docker/traffic-shaping-tool.dockerfile . -t tremor-runtime

demo-run:
	-docker-compose -f demo/demo.yaml rm -fsv
	-docker-compose -f demo/demo.yaml up
	-docker-compose -f demo/demo.yaml rm -fsv
