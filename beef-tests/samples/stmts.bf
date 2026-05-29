var sum = 0;
for (var i = 0; i < 10; i++)
	sum += i * 2;
if (sum > 50)
	Console.WriteLine("big");
else
	Console.WriteLine("small");
return sum;
