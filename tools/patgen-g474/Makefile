# patgen-g474: pattern generator for DSViewR hardware tests (NUCLEO-G474RE)
CUBE_G4 ?= $(HOME)/STM32Cube/Repository/STM32Cube_FW_G4_V1.6.3
PROGRAMMER ?= STM32_Programmer_CLI
CC = arm-none-eabi-gcc
OBJCOPY = arm-none-eabi-objcopy
CFLAGS = -mcpu=cortex-m4 -mthumb -mfloat-abi=soft -O2 -g -Wall -Wextra -std=c11 \
         -ffreestanding -ffunction-sections -fdata-sections -DSTM32G474xx \
         -I$(CUBE_G4)/Drivers/CMSIS/Device/ST/STM32G4xx/Include -I$(CUBE_G4)/Drivers/CMSIS/Include
LDFLAGS = -nostdlib -T g474.ld -Wl,--gc-sections -Wl,-Map=build/patgen.map
BUILD = build

all: $(BUILD)/patgen.bin

$(BUILD)/patgen.elf: main.c g474.ld Makefile
	@mkdir -p $(BUILD)
	$(CC) $(CFLAGS) $(LDFLAGS) main.c -lgcc -o $@
	arm-none-eabi-size $@

$(BUILD)/patgen.bin: $(BUILD)/patgen.elf
	$(OBJCOPY) -O binary $< $@

flash: $(BUILD)/patgen.bin
	$(PROGRAMMER) -c port=SWD -w $< 0x08000000 -v -rst

clean:
	rm -rf $(BUILD)

.PHONY: all flash clean
