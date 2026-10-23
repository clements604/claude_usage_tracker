OPENSCAD ?= openscad
FLAGS = --backend=manifold --export-format=binstl

all: case_base.stl case_lid.stl case_plate.stl
	python3 check_overhangs.py $^

case_base.stl: case.scad
	$(OPENSCAD) $(FLAGS) -D 'part="base"' -o $@ $<

case_lid.stl: case.scad
	$(OPENSCAD) $(FLAGS) -D 'part="lid"' -o $@ $<

case_plate.stl: case.scad
	$(OPENSCAD) $(FLAGS) -D 'part="plate"' -o $@ $<

.PHONY: all
