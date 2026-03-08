# Debilidades conocidas / Known Security Weaknesses

Este documento lista las debilidades de seguridad conocidas de hostelD, sus riesgos y mitigaciones disponibles.

---

## 1. identity.key es la llave maestra local

**Riesgo: ALTO**

El archivo `~/.hostelD/identity.key` (64 bytes: 32 secreto + 32 publico) controla todo:
- Descifra todos los chats almacenados localmente (archivos `.enc`)
- Permite hacerse pasar por el usuario en llamadas y mensajes (keypair X25519)
- Deriva la clave de almacenamiento local via `crypto::derive_storage_key`

**Impacto:** Si alguien copia este archivo, tiene acceso completo a la identidad del usuario y puede leer todo el historial de chats.

**Mitigacion:**
- En Unix tiene permisos `0600` (solo el usuario propietario)
- En Windows depende de los permisos NTFS de la carpeta del usuario
- Cifrado de disco completo (BitLocker/LUKS) protege contra acceso fisico

**Limitacion:** No es posible restringir el acceso por aplicacion en escritorio (Windows/Linux/macOS). Cualquier proceso ejecutandose como el usuario puede leer el archivo. Solo iOS/Android proveen aislamiento por app real.

---

## 2. Llamadas 1:1 — MITM sin verificacion

**Riesgo: MEDIO**

El handshake de llamada usa X25519 efimero sin autenticacion de identidad. Un atacante en la red puede hacer MITM si los usuarios no verifican el codigo `XXXX-XXXX`.

**Flujo de ataque:**
1. Atacante intercepta los paquetes `HELLO` de ambos peers
2. Establece dos sesiones separadas (una con cada peer)
3. Retransmite el audio entre ambas sesiones
4. Cada peer tiene un shared secret diferente, pero si no comparan el codigo, no lo detectan

**Mitigacion:** Verificar el codigo `XXXX-XXXX` en voz al inicio de la llamada. Si coincide, no hay MITM posible (los shared secrets serian diferentes con un atacante en medio).

**Limitacion:** La verificacion es manual y opcional. Muchos usuarios no la realizan.

---

## 3. Grupos — Clave simetrica compartida

**Riesgo: MEDIO**

Todos los miembros del grupo comparten la misma `group_key` (256-bit ChaCha20-Poly1305). Cualquier miembro puede:
- Descifrar todo el audio de las llamadas del grupo
- Leer todos los mensajes de chat del grupo
- Ver screen share y webcam de otros miembros

**Impacto:** La seguridad del grupo es tan fuerte como su miembro mas debil.

**Mitigacion:** Seleccionar cuidadosamente a los miembros del grupo.

---

## 4. Rotacion de clave al expulsar miembro

**Riesgo: MEDIO**

Cuando un miembro es expulsado, se rota la `group_key`. Sin embargo:

### 4a. Ventana de vulnerabilidad
Entre el kick y que todos los miembros reciban la nueva clave, el miembro expulsado todavia puede descifrar trafico encriptado con la clave vieja. La `previous_key` se mantiene como fallback para peers desactualizados.

### 4b. Miembros offline
Si un miembro esta offline cuando ocurre la rotacion:
- Seguira usando la clave vieja hasta que reciba la actualizacion (via `PKT_GRP_UPDATE`)
- El sistema intenta descifrar con la clave anterior como fallback
- La actualizacion se entrega cuando el miembro se conecta y un peer con la nueva clave esta online

### 4c. Solo offline + miembro expulsado online
Si el unico peer online es el miembro expulsado y otro miembro desactualizado:
- Ambos tienen la clave vieja y pueden comunicarse temporalmente
- No hay forma de evitar esto sin servidor central
- Cuando un peer con la nueva clave aparezca, el miembro desactualizado recibira la rotacion

### 4d. No hay forward secrecy en grupo
Si el miembro expulsado grabo paquetes encriptados con la clave vieja, siempre podra descifrarlos. La rotacion solo protege trafico **futuro**.

### 4e. Split-brain con kicks simultaneos
Si dos admins expulsan a dos miembros diferentes al mismo tiempo, ambos generan rotaciones con `key_version` N+1. Se resuelve parcialmente por `key_version` (la version mas alta gana en la actualizacion), pero si ambos generan la misma version, el ultimo update que llegue sobrescribe al anterior.

---

## 5. GRP_HELLO no autenticado

**Riesgo: BAJO**

El paquete `GRP_HELLO` (para unirse a llamadas de grupo) no esta encriptado — contiene un `group_id` y una pubkey dummy. Un atacante que conozca el `group_id` puede enviar HELLOs.

**Impacto limitado:**
- Sin la `group_key`, no puede descifrar paquetes de voz/chat
- Sin la `group_key`, no puede enviar paquetes validos (se ignoran)
- El lider puede registrarlo como peer si la IP coincide con un miembro, pero no aparece en la UI

**Mitigacion:** El `group_id` es un valor aleatorio de 128 bits, dificil de adivinar.

---

## 6. Distribucion de invitaciones

**Riesgo: MEDIO**

La `group_key` viaja en un paquete `PKT_GRP_INVITE` encriptado con la sesion E2E 1:1 entre quien invita y el invitado. Si esa sesion 1:1 fue comprometida por MITM (ver punto 2), el atacante obtiene la `group_key`.

**Mitigacion:** Verificar el contacto (codigo `XXXX-XXXX`) al menos una vez antes de invitarlo al grupo.

---

## 7. Miembro malicioso re-comparte la clave

**Riesgo: BAJO (no tecnico)**

Un miembro activo del grupo podria copiar la `group_key` y compartirla fuera de hostelD con personas no autorizadas. Esto es un problema de confianza humana, no tecnico.

**Mitigacion:** Solo invitar personas de confianza. Expulsar y rotar clave si se sospecha filtracion.

---

## 8. Chats locales sin proteccion contra acceso como usuario

**Riesgo: MEDIO**

Los archivos `.enc` en `~/.hostelD/chats/` y `~/.hostelD/groups/chats/` estan encriptados con ChaCha20-Poly1305, pero la clave se deriva del `identity.key` que esta en el mismo directorio.

**Impacto:** Cualquier proceso corriendo como el usuario puede leer el `identity.key`, derivar la clave de almacenamiento, y descifrar todos los chats.

**Mitigacion:** Cifrado de disco completo + no ejecutar software no confiable.

---

## 9. Metadatos de red visibles

**Riesgo: BAJO**

hostelD usa UDP sobre IPv6. Aunque el contenido esta encriptado, un observador de red puede ver:
- Que dos IPs se estan comunicando
- Cuando inicia y termina una llamada (por el patron de paquetes)
- Tamano aproximado del grupo (por la cantidad de paquetes)
- Que se esta compartiendo pantalla (paquetes mas grandes y frecuentes)

**Mitigacion:** Usar VPN o red overlay (como Tailscale/ZeroTier, que hostelD ya soporta para IPv6).

---

## 10. Firewall bypass por rate limiting

**Riesgo: BAJO**

El sistema de firewall (`firewall.rs`) implementa rate limiting (>200 pkt/sec = strike, 5 strikes = ban). Un atacante podria mantenerse justo por debajo del umbral para enviar trafico no deseado sin ser baneado.

**Impacto limitado:** Sin la clave de sesion, los paquetes se descartarian al fallar la desencriptacion.

---

## 11. Sin rotacion de clave al salir voluntariamente

**Riesgo: BAJO**

Actualmente la rotacion de `group_key` solo ocurre cuando un admin **expulsa** a un miembro. Si un miembro sale voluntariamente del grupo, la clave no se rota.

**Razon:** Un miembro que sale voluntariamente probablemente no tiene intencion maliciosa. Pero podria seguir descifrando trafico si intercepta paquetes.

**Posible mejora futura:** Rotar la clave tambien en salidas voluntarias.

---

## 12. group.json no encriptado en disco

**Riesgo: MEDIO**

Los archivos de grupo (`~/.hostelD/groups/{id}.json`) se guardan en texto plano (JSON). Contienen:
- La `group_key` en formato hex
- Lista de miembros con pubkeys, nicknames, IPs
- Configuracion de canales

**Impacto:** Acceso al filesystem del usuario expone las claves de todos los grupos.

**Mitigacion:** Cifrado de disco completo. Posible mejora futura: encriptar los archivos de grupo con la `identity.key`.

---

## 13. Sin expiracion de previous_key

**Riesgo: BAJO**

La `previous_key` se mantiene indefinidamente para fallback de peers desactualizados. Un miembro expulsado que obtiene acceso a paquetes encriptados con la clave nueva no puede descifrarlos, pero la `previous_key` permite descifrar trafico de peers lentos en actualizar.

**Posible mejora futura:** Expirar `previous_key` despues de un periodo razonable (ej: 24 horas).

---

## Resumen de prioridades

| # | Debilidad | Riesgo | Mitigable por el usuario |
|---|-----------|--------|--------------------------|
| 1 | identity.key expuesta | ALTO | Cifrado de disco |
| 12 | group.json en texto plano | MEDIO | Cifrado de disco |
| 2 | MITM sin verificacion | MEDIO | Verificar codigo XXXX-XXXX |
| 3 | Clave simetrica compartida en grupo | MEDIO | Seleccionar miembros |
| 4 | Ventana de rotacion de clave | MEDIO | Inherente a P2P |
| 6 | Invitacion comprometida | MEDIO | Verificar contacto primero |
| 8 | Chats locales accesibles | MEDIO | Cifrado de disco |
| 9 | Metadatos de red | BAJO | VPN |
| 5 | GRP_HELLO no autenticado | BAJO | group_id aleatorio |
| 7 | Re-compartir clave | BAJO | Confianza |
| 10 | Rate limit bypass | BAJO | Clave requerida |
| 11 | Sin rotacion en salida voluntaria | BAJO | Mejora futura |
| 13 | previous_key sin expiracion | BAJO | Mejora futura |
