# patgen-g474: pattern generator for DSViewR hardware tests

Firmware for a NUCLEO-G474RE that drives known signals into a DSLogic, so
`dsviewr-cli hwsig` can check the input stage, triggers and decoders against
exact expectations (the DSLogic's own test mode only covers its internal
counter).

- **Wiring:** any four of PA0/1/4-12/15, PB0-2/4-7/9-15 to DSLogic channels
  0-3, plus GND. `hwsig` finds which pin is on which channel itself (`scan`).
  PA5/PB6 carry the timer PWM, PB6 USART1_TX, PA5/PA7 SPI1 SCK/MOSI (PA4 CS
  if wired); everything else is bit-banged on the mapped pins.
- **Clock:** 170 MHz from the board's 24 MHz crystal (`id` says `hse24`),
  else HSI (±1 %; `hwsig` widens its timing tolerances).
- **Build and flash** (arm-none-eabi-gcc, STM32CubeG4's CMSIS headers,
  STM32_Programmer_CLI over the on-board ST-LINK):

  ```sh
  make CUBE_G4=~/STM32Cube/Repository/STM32Cube_FW_G4_V1.6.3 flash
  ```

- **Protocol:** text lines on the ST-LINK virtual COM port (115200 8N1),
  replies `ok …`/`err …`; the command list is at the top of `main.c`.
  Continuous patterns run until the next byte arrives.

```sh
dsviewr-cli hwsig --patgen /dev/ttyACM0 --out report.json
dsviewr-cli hwsig --only uart,spi --work /tmp/hwsig
```
