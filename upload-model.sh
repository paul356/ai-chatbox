#!/usr/bin/bash

for dir in `ls`; do
    if [ -d $dir ]; then
        for f in `ls $dir`; do
            if [ -f ./$dir/$f ]; then
                curl -X POST -T ./$dir/$f http://192.168.1.5/upload/$dir/$f;
            fi
        done
    fi
done
