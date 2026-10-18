# FFTA Europa (AFXP): perfil de parches

ROM: `Final Fantasy Tactics Advance (Europe) (En,Fr,De,Es,It)`, código `AFXP`,
16 MiB, SHA1 `9efaf328cbbcbc830be14940e42e4b92d90dfb58`,
SHA256 `8fab1e390cb9d5e695bfdb86e81246892e1aa8c0cb4014db8235ff76538ea632`.
No se incluye ningún byte de la ROM; solo direcciones y hashes.

El recompilador es genérico: sin perfil la ROM europea ya se instalaba como
"Unpatched" (sin parches BG/SPR/VIS/MOVEMENT/STAGE). El perfil `EUR` de
`crates/gba-core/src/ffta_rom.rs` activa los mismos parches que la USA.

## Correspondencia USA → EUR

| Elemento | USA | EUR | Método |
| --- | --- | --- | --- |
| Productor OBJ 1 (Thumb) | 080014C8 | 08001500 | MEDIDO: SHA256 USA tras re-apuntar 1 literal |
| Productor OBJ 2 (Thumb) | 080016D4 | 0800170C | MEDIDO: SHA256 USA tras re-apuntar 1 literal |
| Tabla de dimensiones OBJ | 08149510 | 0814CED8 | MEDIDO: contenido = tabla de dimensiones GBA |
| Llamadores BL de 1 | 08001D80, 1EA4, 1FC0, 20DC | 08001DB8, 1EDC, 1FF8, 2114 | MEDIDO: +0x38 |
| Llamadores BL de 2 | 08001DEE, 1F0A, 2026, 2142 | 08001E26, 1F42, 205E, 217A | MEDIDO: +0x38 |
| Rutina de batalla (copia IWRAM) | ROM A3915C | ROM D96F68 | MEDIDO: idéntica byte a byte |
| Rutina de visibilidad (copia IWRAM) | ROM A38EA0 | ROM D96CAC | MEDIDO: idéntica byte a byte |
| Tabla de dimensiones IWRAM | ROM A398DC | ROM D976E8 | MEDIDO: idéntica |
| Destinos IWRAM 030062EC/030065A8/03006D28, bancos 03000020/28/30 | — | iguales | MEDIDO: las copias idénticas contienen esas direcciones absolutas |
| Estructura de escenario 02007F40 (+08,+0A,+14,+18,+28,+2A) | — | igual | MEDIDO: accesos Thumb con los mismos offsets y anchos |
| Planos de movimiento 02003CB0/02005CB0, VRAM 06005000/06005800 | — | literales presentes | MEDIDO estáticamente; el parche además valida cada palabra contra VRAM |
| Lista del panel de stats | 083916FA | 08395102 | MEDIDO: única lista de 4 piezas con tiles 1F0/200/208/208 en la ROM, un solo literal (0802D800); en ejecución llega a 08001500 desde 08001DBD como semitransparente, paleta 11 |
| Rango de comprobación del terreno | 1AF08..1B5D0 | — | Sin equivalente idéntico; el SHA256 de la imagen completa ya fija el código |

Las dos funciones de ROM son exactamente los bytes USA desplazados +0x38;
la única diferencia es el literal de la tabla de dimensiones (una vez en el
literal pool de cada función). Re-apuntándolo a 08149510 se obtiene
el SHA256 USA publicado en `ffta_rom.rs`, lo que prueba la equivalencia.

## Comprobación

```sh
python3 -I scripts/ffta_eur_verify.py "Final Fantasy Tactics Advance (Europe) (En,Fr,De,Es,It).gba"
```

Debe terminar con `EUR PROFILE VERIFIED`. Pruebas Rust con la ROM del usuario:

```sh
FFTA_TEST_ROM=/ruta/absoluta/ffta-eur.gba cargo test -p gba-core --lib -- --ignored --nocapture
FFTA_TEST_ROM=/ruta/absoluta/ffta-eur.gba cargo test -p installer -- --ignored --nocapture
```

## Validación realizada (Linux, Rust 1.99.1, gcc 13.3, 2026-10-18)

- MEDIDO: `cargo test` de gba-core (88 ok), installer (6 ok) y recomp-core (3 ok).
- MEDIDO: tests con la ROM EUR (`--ignored`): decodificador de sprites, rutina
  de batalla IWRAM y visibilidad dan resultados IDÉNTICOS al código original
  ejecutado en el intérprete (24 + 24 + 70 casos). El test del instalador
  acepta la imagen EUR y rechaza la copia con un byte cambiado.
- MEDIDO: con el código de `main` la ROM EUR no activaba ningún parche; con
  este perfil se activan BG, SPR, VIS, MOVEMENT y STAGE.
- MEDIDO: replay de 13400 frames (selección de idioma, nombre, colegio y
  batalla de bolas de nieve con el cursor sobre unidades):
  - `recomp verify` (240 px): MATCH, intérprete = recompilado (8be97e6249839e2b).
  - `FFTA_WIDESCREEN=32:9`: frame ancho final de `runc` (nativo, 28859 bloques)
    idéntico byte a byte al de `frames` (intérprete).
  - Parches activos frente a desactivados: los 240 px centrales son idénticos;
    solo cambian los márgenes (sin repetición del mapa ni de la cuadrícula de
    movimiento). Con `stats_list` el fondo rectangular del panel llega al borde.
- NO MEDIDO: sesión jugable con ventana y audio, y la ROM USA (no disponible
  aquí) tras el cambio a perfiles; sus constantes no se han modificado.

## Pendiente

- Los diagnósticos de `ffta_log.rs` y el trazado de `emit.rs` bajo
  `FFTA_OAM_LOG` siguen usando direcciones USA; con EUR no registran nada.
