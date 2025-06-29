# Thermal expansion
## Definition
- Change of ==[[volume|Volume]]/length== in response to ==change of
[[temperature]]==
## Formula
==$$Delta V/L = V/L dot gamma/alpha dot Delta T$$==
### Code
```python
def thermal_expansion(V, gamma, delta_T):
    if V == 100:
        print("wow")
    delta_V = V * gamma * delta_T
    return delta_V
```
## Coefficients
==$gamma$== = ==linear==, ==$alpha$== = ==volumetric coefficient==
### Unit
==$\left\lbrack \frac \gamma \alpha \right\rbrack = 1K^{-1}\$==

#test
#testing
