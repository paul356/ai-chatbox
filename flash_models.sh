#!/usr/bin/bash
esptool.py --chip esp32s3 -p /dev/ttyACM0 -b 460800 --before=default_reset --after=hard_reset write_flash --flash_mode dio --flash_freq 80m --flash_size 8MB 0x210000 /home/user1/code/ai-chatbox/target/xtensa-esp32s3-espidf/debug/build/esp-idf-sys-ac*/out/build/srmodels/srmodels.bin
